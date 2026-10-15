//! Materialize recorded composition facts against their original source representation.
//! The composer owns fact validation; the assembler owns the resulting hierarchy.
use crate::{
    apply_structure_composition, assemble_structure_annotations, AppliedComposition,
    CompositionAnnotation, CompositionLine, CompositionRequest, DocumentStructure, ScalarRange,
    ScalarText, SourceSpan,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionProjectionRequest {
    /// Original source export. Unknown producer evidence is preserved separately.
    pub structure: Value,
    pub composition: CompositionRequest,
    /// Original nodes selected for retention; authored block/table/note families are replaced.
    pub retained_node_ids: Vec<String>,
}

/// Non-queryable producer/artifact evidence, overlaid by identity by artifact writers.
#[derive(Debug, Default, Serialize)]
pub struct CompositionStructureEvidence {
    pub fields: Map<String, Value>,
    pub nodes: BTreeMap<String, Map<String, Value>>,
    pub notes: BTreeMap<String, Map<String, Value>>,
}

#[derive(Debug, Serialize)]
pub struct ProjectedStructureComposition {
    pub native_facts: crate::CompositionNativeFacts,
    pub structure: DocumentStructure,
    pub composition: AppliedComposition,
    pub source_trace: Vec<Value>,
    pub unconfirmed_native_witnesses: Vec<Value>,
    pub structure_evidence: CompositionStructureEvidence,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProjectedSpan {
    line_id: String,
    page_index: usize,
    range: ScalarRange,
    rendered_range: ScalarRange,
}

#[derive(Clone, Debug, Serialize)]
struct RenderedSpan {
    #[serde(flatten)]
    span: SourceSpan,
    range: ScalarRange,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WitnessRun {
    start: usize,
    end: usize,
    source_start: usize,
    source_end: usize,
}

struct SourceLine {
    text: String,
    range: ScalarRange,
}

struct Projection<'a> {
    composition: &'a AppliedComposition,
    lines: HashMap<&'a str, &'a CompositionLine>,
    ranges: HashMap<String, ScalarRange>,
    rendered: HashMap<String, Vec<RenderedSpan>>,
    originals: BTreeMap<String, SourceLine>,
    witnesses: HashMap<String, Vec<WitnessRun>>,
    descendants: HashMap<String, Vec<String>>,
}

fn encoded<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}
fn decoded<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}
fn spans(value: Option<&Value>) -> Result<Vec<SourceSpan>> {
    decoded(value.cloned().unwrap_or_else(|| json!([])))
}
fn anchor(span: &SourceSpan) -> String {
    format!("{}@{}", span.line_id, span.start)
}
fn unique<T: Clone + Eq + std::hash::Hash>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}
fn bounds(ranges: impl IntoIterator<Item = ScalarRange>, fallback: usize) -> ScalarRange {
    let ranges: Vec<_> = ranges.into_iter().collect();
    ScalarRange {
        start: ranges
            .iter()
            .map(|range| range.start)
            .min()
            .unwrap_or(fallback),
        end: ranges
            .iter()
            .map(|range| range.end)
            .max()
            .unwrap_or(fallback),
    }
}
fn merged_ranges(mut ranges: Vec<ScalarRange>) -> Vec<ScalarRange> {
    ScalarRange::normalize_union(&mut ranges);
    ranges
}

impl<'a> Projection<'a> {
    fn new(
        composition: &'a AppliedComposition,
        baseline: &DocumentStructure,
    ) -> Result<(Self, String, String, Vec<Value>)> {
        let lines: HashMap<_, _> = composition
            .lines
            .iter()
            .map(|line| (line.id.as_str(), line))
            .collect();
        let source_text = ScalarText::new(&baseline.text);
        let mut originals = BTreeMap::new();
        for fragment in &baseline.source_fragments {
            let text = source_text
                .slice_utf16(fragment.range.start..fragment.range.end)
                .ok_or("Original source fragment splits a character or is outside text")?;
            if originals
                .insert(
                    fragment.id.clone(),
                    SourceLine {
                        text: text.into(),
                        range: fragment.range,
                    },
                )
                .is_some()
            {
                return Err("Repeated original source fragment".into());
            }
        }
        // Older/native markup exports can identify unchanged source units directly
        // by their evidence ranges without a separate fragment inventory.
        if originals.is_empty() {
            for line in &composition.lines {
                let Some(range) = line.evidence.get("range") else {
                    continue;
                };
                let range: ScalarRange = decoded(range.clone())?;
                let candidate = line
                    .evidence
                    .get("original_text")
                    .and_then(Value::as_str)
                    .unwrap_or(&line.text);
                if source_text.slice_utf16(range.start..range.end) != Some(candidate) {
                    continue;
                }
                let id = line
                    .evidence
                    .get("source_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&line.id);
                if let Some(previous) = originals.get(id) {
                    if previous.range != range || previous.text != candidate {
                        return Err("Conflicting original source lineage".into());
                    }
                } else {
                    originals.insert(
                        id.to_owned(),
                        SourceLine {
                            text: candidate.into(),
                            range,
                        },
                    );
                }
            }
        }
        let mut ranges = HashMap::new();
        let mut pages = Vec::new();
        let mut chunks = Vec::new();
        let mut cursor = 0;
        let mut page_numbers = HashSet::new();
        let mut page_sources = baseline
            .nodes
            .iter()
            .filter(|node| node.kind == crate::NodeKind::Page)
            .map(|node| {
                node.page_indexes
                    .first()
                    .copied()
                    .map(|page| (page + 1, Some(node)))
                    .ok_or_else(|| "Page has no physical index".to_owned())
            })
            .collect::<Result<Vec<_>>>()?;
        if page_sources.is_empty() {
            page_sources = composition
                .lines
                .iter()
                .map(|line| line.page)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|page| (page, None))
                .collect();
        }
        for (page, original) in page_sources {
            if !page_numbers.insert(page) {
                return Err("Repeated physical page".into());
            }
            let ids: Vec<_> = composition
                .reading_order
                .iter()
                .filter(|id| lines[id.as_str()].page == page)
                .cloned()
                .collect();
            let start = cursor;
            let mut content = Vec::new();
            for id in &ids {
                let text = &lines[id.as_str()].text;
                let end = cursor + text.encode_utf16().count();
                ranges.insert(id.clone(), ScalarRange { start: cursor, end });
                cursor = end + 1;
                content.push(text.as_str());
            }
            let content = content.join("\n");
            if ids.is_empty() {
                cursor += 1;
            }
            if let Some(original) = original {
                let mut value = encoded(original)?;
                value["line_ids"] = json!(ids);
                value["range"] =
                    json!({"start":start,"end":start + content.encode_utf16().count()});
                value["source"] = json!("model");
                for field in [
                    "ranges",
                    "extent",
                    "heading_range",
                    "level",
                    "content_start",
                    "marker_range",
                    "rendered_range",
                    "rendered_ranges",
                    "rendered_membership",
                ] {
                    value.as_object_mut().unwrap().remove(field);
                }
                pages.push(value);
            }
            chunks.push(content);
        }
        if ranges.len() != lines.len() {
            return Err("Composition source names an absent physical page".into());
        }
        let mut projection = Self {
            composition,
            lines,
            ranges,
            rendered: HashMap::new(),
            originals,
            witnesses: HashMap::new(),
            descendants: HashMap::new(),
        };
        projection.read_lineage()?;
        let mut manuscript = String::new();
        let mut cursor = 0;
        for member in &composition.manuscript_spans {
            manuscript.push_str(&member.separator);
            cursor += member.separator.encode_utf16().count();
            let text = projection.logical_span(
                &member.span.line_id,
                member.span.start,
                member.span.end,
            )?;
            let end = cursor + text.encode_utf16().count();
            manuscript.push_str(&text);
            projection
                .rendered
                .entry(member.span.line_id.clone())
                .or_default()
                .push(RenderedSpan {
                    span: member.span.clone(),
                    range: ScalarRange { start: cursor, end },
                });
            cursor = end;
        }
        for id in projection.lines.keys() {
            if !projection.rendered.contains_key(*id) {
                return Err("Source unit has no manuscript membership".into());
            }
        }
        for page in &mut pages {
            let ids: Vec<String> = decoded(page["line_ids"].clone())?;
            let membership: Vec<_> = ids
                .iter()
                .flat_map(|id| projection.rendered[id].iter().map(|span| span.range))
                .collect();
            page["rendered_membership"] = json!(membership);
            page["rendered_ranges"] = json!(membership);
        }
        Ok((projection, chunks.join("\n"), manuscript, pages))
    }

    fn read_lineage(&mut self) -> Result<()> {
        for id in &self.composition.reading_order {
            let line = self.lines[id.as_str()];
            let source_id = match line.evidence.get("source_id") {
                Some(Value::Null) => None,
                Some(Value::String(id)) => Some(id.clone()),
                Some(_) => return Err("Source lineage identity must be a string or null".into()),
                None => self.originals.contains_key(id).then(|| id.clone()),
            };
            let Some(source_id) = source_id else {
                continue;
            };
            let original = self
                .originals
                .get(&source_id)
                .ok_or("Source lineage names absent original fragment")?;
            let old = ScalarText::new(&original.text);
            let current = ScalarText::new(&line.text);
            if line
                .evidence
                .get("original_text")
                .is_some_and(|value| value.as_str() != Some(original.text.as_str()))
            {
                return Err("Lineage original text differs from the baseline fragment".into());
            }
            let extent: Option<[usize; 2]> = line
                .evidence
                .get("source_range")
                .filter(|value| !value.is_null())
                .map(|value| decoded(value.clone()))
                .transpose()?;
            if extent.is_some_and(|[start, end]| start > end || end > old.len()) {
                return Err("Source lineage extent is outside original fragment".into());
            }
            let runs: Vec<WitnessRun> = if let Some(value) = line.evidence.get("witness_mapping") {
                if value.is_null() {
                    Vec::new()
                } else {
                    decoded(value.clone())?
                }
            } else if let Some([start, end]) = extent {
                if old.slice(start..end) == Some(line.text.as_str()) {
                    vec![WitnessRun {
                        start: 0,
                        end: current.len(),
                        source_start: start,
                        source_end: end,
                    }]
                } else {
                    Vec::new()
                }
            } else if original.text == line.text {
                vec![WitnessRun {
                    start: 0,
                    end: current.len(),
                    source_start: 0,
                    source_end: old.len(),
                }]
            } else {
                Vec::new()
            };
            let mut previous = None;
            for run in &runs {
                if run.start > run.end
                    || run.end > current.len()
                    || run.source_start > run.source_end
                    || run.source_end > old.len()
                    || run.end - run.start != run.source_end - run.source_start
                    || old.slice(run.source_start..run.source_end)
                        != current.slice(run.start..run.end)
                    || previous.is_some_and(|(end, source_end)| {
                        run.start < end || run.source_start < source_end
                    })
                {
                    return Err("Invalid unchanged source witness mapping".into());
                }
                previous = Some((run.end, run.source_end));
            }
            self.witnesses.insert(id.clone(), runs);
            self.descendants
                .entry(source_id)
                .or_default()
                .push(id.clone());
        }
        Ok(())
    }

    fn logical_span(&self, id: &str, start: usize, end: usize) -> Result<String> {
        let line = self.lines.get(id).ok_or("Manuscript source absent")?;
        let source = ScalarText::new(&line.text);
        if start > end || end > source.len() {
            return Err("Manuscript span outside source".into());
        }
        if start == end {
            return Ok(String::new());
        }
        let edits = self
            .composition
            .manuscript_edits
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut output = String::new();
        let mut cursor = start;
        for (lo, hi, replacement) in edits {
            if (*lo < start && start < *hi) || (*lo < end && end < *hi) {
                return Err("Manuscript boundary crosses normalization".into());
            }
            if *hi < start
                || (*hi == start && lo != hi)
                || *lo > end
                || (*lo == end && !(lo == hi && end == source.len()))
            {
                continue;
            }
            if *lo < start {
                continue;
            }
            output.push_str(
                source
                    .slice(cursor..*lo)
                    .ok_or("Invalid manuscript normalization boundary")?,
            );
            output.push_str(replacement);
            cursor = *hi;
        }
        output.push_str(
            source
                .slice(cursor..end)
                .ok_or("Invalid manuscript normalization boundary")?,
        );
        Ok(output)
    }

    fn project_span(&self, span: &SourceSpan) -> Result<Vec<ProjectedSpan>> {
        let line = self
            .lines
            .get(span.line_id.as_str())
            .ok_or("Annotation source absent")?;
        let text = ScalarText::new(&line.text);
        if span.start >= span.end || span.end > text.len() {
            return Err("Annotation range outside corrected source".into());
        }
        let mut members: Vec<_> = self.rendered[&span.line_id].iter().collect();
        members.sort_by_key(|member| member.span.start);
        let mut result = Vec::new();
        for member in members {
            let start = span.start.max(member.span.start);
            let end = span.end.min(member.span.end);
            if start >= end {
                continue;
            }
            result.push(ProjectedSpan {
                line_id: span.line_id.clone(),
                page_index: line.page - 1,
                range: ScalarRange {
                    start: self.ranges[&span.line_id].start + text.utf16(start),
                    end: self.ranges[&span.line_id].start + text.utf16(end),
                },
                rendered_range: ScalarRange {
                    start: member.range.start
                        + self
                            .logical_span(&span.line_id, member.span.start, start)?
                            .encode_utf16()
                            .count(),
                    end: member.range.start
                        + self
                            .logical_span(&span.line_id, member.span.start, end)?
                            .encode_utf16()
                            .count(),
                },
            });
        }
        Ok(result)
    }

    fn source_label(&self, spans: &[SourceSpan]) -> Result<String> {
        let mut label = String::new();
        for span in spans {
            let line = self
                .lines
                .get(span.line_id.as_str())
                .ok_or("Label source absent")?;
            label.push_str(
                ScalarText::new(&line.text)
                    .slice(span.start..span.end)
                    .ok_or("Label outside source")?,
            );
        }
        Ok(label)
    }

    fn project_spans(&self, spans: &[SourceSpan]) -> Result<Vec<ProjectedSpan>> {
        spans
            .iter()
            .map(|span| self.project_span(span))
            .collect::<Result<Vec<_>>>()
            .map(|parts| parts.into_iter().flatten().collect())
    }

    fn project(&self, value: &Value) -> Result<Value> {
        match value {
            Value::Object(object)
                if object.len() == 3
                    && object.contains_key("line_id")
                    && object.contains_key("start")
                    && object.contains_key("end") =>
            {
                encoded(self.project_span(&decoded(value.clone())?)?)
            }
            Value::Object(object) => object
                .iter()
                .map(|(key, value)| {
                    Ok((
                        key.clone(),
                        if key == "native_witness" {
                            value.clone() // Original evidence keeps its declared baseline coordinates.
                        } else {
                            self.project(value)?
                        },
                    ))
                })
                .collect::<Result<Map<_, _>>>()
                .map(Value::Object),
            Value::Array(values) => {
                let mut result = Vec::new();
                for value in values {
                    let projected = self.project(value)?;
                    if value.as_object().is_some_and(|object| {
                        object.len() == 3
                            && object.contains_key("line_id")
                            && object.contains_key("start")
                            && object.contains_key("end")
                    }) {
                        result.extend(projected.as_array().unwrap().clone());
                    } else {
                        result.push(projected);
                    }
                }
                Ok(Value::Array(result))
            }
            _ => Ok(value.clone()),
        }
    }

    fn anchor(&self, id: &str) -> Result<usize> {
        let (id, at) = if self.lines.contains_key(id) {
            (id, 0)
        } else {
            let (id, at) = id.rsplit_once('@').ok_or("Expected source anchor")?;
            (
                id,
                at.parse::<usize>().map_err(|_| "Invalid source anchor")?,
            )
        };
        let line = self.lines.get(id).ok_or("Absent source anchor")?;
        let coordinates = ScalarText::new(&line.text);
        if at >= coordinates.len() {
            return Err("Source anchor outside text".into());
        }
        Ok(self.ranges[id].start + coordinates.utf16(at))
    }

    fn owner(&self, span: &SourceSpan) -> Result<String> {
        self.composition
            .groups
            .iter()
            .find(|group| {
                group.spans.iter().any(|member| {
                    member.line_id == span.line_id
                        && member.start <= span.start
                        && span.start < member.end
                })
            })
            .and_then(|group| group.ownership.as_ref())
            .map(|owner| owner.document_id.clone())
            .ok_or_else(|| "Annotation has no owning source block".into())
    }

    fn node(
        &self,
        id: &str,
        kind: &str,
        spans: &[SourceSpan],
        origin: &str,
        owner: Value,
    ) -> Result<Value> {
        let precise = self.project_spans(spans)?;
        let ids = unique(spans.iter().map(|span| span.line_id.clone()));
        let pages: BTreeSet<_> = ids
            .iter()
            .map(|id| self.lines[id.as_str()].page - 1)
            .collect();
        let ranges: Vec<_> = precise.iter().map(|part| part.range).collect();
        let rendered: Vec<_> = precise.iter().map(|part| part.rendered_range).collect();
        let document = spans.first().map(|span| self.owner(span)).transpose()?;
        Ok(
            json!({"id":id,"kind":kind,"range":bounds(ranges.iter().copied(),0),"ranges":ranges,
            "origin_id":origin,"source":"model","line_ids":ids,"page_indexes":pages,
            "rendered_ranges":rendered,"rendered_membership":rendered,"document_id":document,
            "parent_id":null,"ownership":owner}),
        )
    }
}

fn annotation_owner(annotation: &CompositionAnnotation) -> Value {
    annotation
        .evidence
        .get("ownership")
        .cloned()
        .unwrap_or(Value::Null)
}
fn range_values(parts: &[ProjectedSpan]) -> Vec<ScalarRange> {
    parts.iter().map(|part| part.range).collect()
}

struct ProjectedFacts {
    nodes: Vec<Value>,
    notes: Vec<Value>,
    fields: Vec<Value>,
    annotations: Vec<Value>,
    documents: Vec<Value>,
    scopes: Vec<Value>,
    navigation: Vec<Value>,
}

fn project_facts(
    projection: &Projection<'_>,
    baseline: &DocumentStructure,
    native_navigation: &[crate::NativeNavigationWitness],
) -> Result<ProjectedFacts> {
    let composition = projection.composition;
    let origin = baseline
        .origins
        .first()
        .map(|origin| origin.id.as_str())
        .unwrap_or("source");
    let positions: HashMap<_, _> = composition
        .reading_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let by_id: HashMap<_, _> = composition
        .groups
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let mut logical = Vec::new();
    let mut group_nodes = HashMap::new();
    for (first, fragments) in &composition.logical_blocks {
        let first_group = by_id[first.as_str()];
        let members: Vec<_> = fragments
            .iter()
            .flat_map(|id| by_id[id.as_str()].spans.iter().cloned())
            .collect();
        for fragment in fragments {
            group_nodes.insert(fragment.as_str(), first.as_str());
        }
        logical.push((first_group, members));
    }
    logical.sort_by_key(|(_, spans)| (positions[spans[0].line_id.as_str()], spans[0].start));
    let mut nodes = Vec::new();
    for (group, members) in &logical {
        let attrs = &group.attributes;
        let kind = if group.kind == "heading" {
            "heading"
        } else {
            attrs
                .get("locator_kind")
                .and_then(Value::as_str)
                .unwrap_or("prose")
        };
        let kind = if matches!(
            kind,
            "prose" | "heading" | "paragraph" | "section" | "list_item"
        ) {
            kind
        } else {
            "prose"
        };
        let mut node =
            projection.node(&group.id, kind, members, origin, encoded(&group.ownership)?)?;
        node["role"] = json!(group.kind);
        let precise = projection.project_spans(members)?;
        node["rendered_range"] = json!(bounds(precise.iter().map(|part| part.rendered_range), 0));
        if group.kind == "heading" && precise.len() == 1 {
            node["heading_range"] = node["range"].clone();
        }
        let markers = spans(attrs.get("marker"))?;
        if !markers.is_empty() {
            let projected = projection.project_spans(&markers)?;
            let label = projection.source_label(&markers)?;
            node["label"] = json!(label);
            node["locator_kind"] = json!(kind);
            node["marker_spans"] = json!(projected);
            if projected.len() == 1 {
                node["marker_range"] = json!(projected[0].range);
            }
        }
        if let Some(parent) = attrs.get("parent").and_then(Value::as_str) {
            node["parent_id"] = json!(group_nodes
                .get(parent)
                .ok_or("Logical block parent absent")?);
        }
        nodes.push(node);
    }
    let annotations =
        crate::composition_annotations::resolve_applied_annotations(composition)?.annotations;
    let mut exported = Vec::new();
    let mut notes = Vec::new();
    let mut fields = Vec::new();
    let mut table_number = 0;
    for annotation in &annotations {
        let attrs = &annotation.attributes;
        let precise = projection.project_spans(&annotation.spans)?;
        let owner = annotation_owner(annotation);
        let mut value = encoded(annotation)?;
        value["document_id"] = json!(projection.owner(&annotation.spans[0])?);
        match annotation.kind.as_str() {
            "note" => {
                let labels = spans(attrs.get("label"))?;
                let label = projection.project_spans(&labels)?;
                let mut bodies = Vec::new();
                for span in &annotation.spans {
                    let mut pieces = vec![(span.start, span.end)];
                    for marker in labels
                        .iter()
                        .filter(|marker| marker.line_id == span.line_id)
                    {
                        pieces = pieces
                            .into_iter()
                            .flat_map(|(start, end)| {
                                [(start, end.min(marker.start)), (start.max(marker.end), end)]
                            })
                            .filter(|(start, end)| start < end)
                            .collect();
                    }
                    bodies.extend(pieces.into_iter().map(|(start, end)| SourceSpan {
                        line_id: span.line_id.clone(),
                        start,
                        end,
                    }));
                }
                let body = projection.project_spans(&bodies)?;
                let kind = attrs
                    .get("kind")
                    .and_then(Value::as_str)
                    .ok_or("Missing note kind")?;
                let has_node = matches!(kind, "footnote" | "endnote");
                if has_node {
                    let mut node = projection.node(
                        &annotation.id,
                        kind,
                        &annotation.spans,
                        origin,
                        owner.clone(),
                    )?;
                    node["label"] = json!(projection.source_label(&labels)?);
                    node["rendered_range"] =
                        json!(bounds(precise.iter().map(|part| part.rendered_range), 0));
                    nodes.push(node);
                }
                let references: Vec<_> = projection
                    .project_spans(&spans(attrs.get("references"))?)?
                    .into_iter()
                    .map(|part| {
                        let mut value = json!(part);
                        value["line_ids"] = json!([part.line_id]);
                        value["page_indexes"] = json!([part.page_index]);
                        value
                    })
                    .collect();
                let start = bounds(precise.iter().map(|part| part.range), 0).start;
                notes.push(json!({"id":annotation.id,"node_id":has_node.then_some(&annotation.id),"ownership":owner,"kind":kind,
                    "label_range":bounds(label.iter().map(|part|part.range),start),"body_range":bounds(body.iter().map(|part|part.range),start),
                    "label_spans":label,"body_spans":body,"label_ranges":range_values(&label),"body_ranges":range_values(&body),
                    "primary_reference":references.first().map(|value|value["range"].clone()),"references":references}));
            }
            "field" => {
                let mut field = json!({"id":annotation.id,"type":attrs["type"],"ownership":owner,"ranges":range_values(&precise),"state":attrs.get("state")});
                for key in ["label", "value", "placeholder"] {
                    field[format!("{key}_ranges")] = json!(range_values(
                        &projection.project_spans(&spans(attrs.get(key))?)?
                    ));
                }
                fields.push(field);
            }
            "table" => {
                let rows: Vec<Vec<Option<Vec<SourceSpan>>>> = decoded(attrs["rows"].clone())?;
                let merges: Vec<[usize; 4]> = decoded(attrs["merges"].clone())?;
                let headers: BTreeSet<_> = decoded::<Vec<usize>>(attrs["header_rows"].clone())?
                    .into_iter()
                    .map(|row| row + 1)
                    .collect();
                table_number += 1;
                let mut table = projection.node(
                    &annotation.id,
                    "table",
                    &annotation.spans,
                    origin,
                    owner.clone(),
                )?;
                table["table_number"] = json!(table_number);
                table["label"] = json!(format!("table:{table_number}"));
                table["row_count"] = json!(rows.len());
                table["column_count"] = json!(rows.first().map_or(0, Vec::len));
                table["merges"] = json!(merges);
                table["header_rows"] = json!(headers);
                nodes.push(table.clone());
                for (row_index, row) in rows.iter().enumerate() {
                    let row_id = format!("{}.row{}", annotation.id, row_index + 1);
                    let mut cell_ids = Vec::new();
                    let mut row_spans = Vec::new();
                    for (column_index, cell) in row.iter().enumerate() {
                        let Some(cell) = cell else {
                            continue;
                        };
                        let cell_id = format!("{row_id}.cell{}", column_index + 1);
                        let merge = merges
                            .iter()
                            .find(|merge| merge[0] == row_index && merge[1] == column_index)
                            .copied()
                            .unwrap_or([row_index, column_index, 1, 1]);
                        let mut node =
                            projection.node(&cell_id, "cell", cell, origin, owner.clone())?;
                        node["parent_id"] = json!(row_id);
                        node["row"] = json!(row_index + 1);
                        node["column"] = json!(column_index + 1);
                        node["label"] = json!(format!(
                            "table:{table_number}/row:{}/col:{}",
                            row_index + 1,
                            column_index + 1
                        ));
                        node["row_span"] = json!(merge[2]);
                        node["column_span"] = json!(merge[3]);
                        node["blank"] = json!(cell.is_empty());
                        if cell.is_empty() {
                            empty_grid_position(&mut node, &table);
                        }
                        nodes.push(node);
                        cell_ids.push(cell_id);
                        row_spans.extend(cell.iter().cloned());
                    }
                    let mut node =
                        projection.node(&row_id, "row", &row_spans, origin, owner.clone())?;
                    node["parent_id"] = json!(annotation.id);
                    node["row"] = json!(row_index + 1);
                    node["label"] = json!(format!("table:{table_number}/row:{}", row_index + 1));
                    node["cell_ids"] = json!(cell_ids);
                    if row_spans.is_empty() {
                        empty_grid_position(&mut node, &table);
                    }
                    nodes.push(node);
                }
            }
            _ => {}
        }
        exported.push(projection.project(&value)?);
    }
    let (documents, scopes) = project_scopes(projection, &annotations, &exported)?;
    let navigation = project_navigation(projection, native_navigation)?;
    let quoted: Vec<_> = exported
        .iter()
        .filter(|value| value["kind"] == "quotation")
        .collect();
    for node in &mut nodes {
        let refs = if let Some(markers) = node.get("marker_spans") {
            decoded::<Vec<ProjectedSpan>>(markers.clone())?
        } else {
            let members = logical
                .iter()
                .find(|(group, _)| Some(group.id.as_str()) == node["id"].as_str())
                .map(|(_, spans)| spans.as_slice())
                .unwrap_or_default();
            projection.project_spans(members)?
        };
        let mut matches = Vec::new();
        for quotation in &quoted {
            let members: Vec<ProjectedSpan> = decoded(quotation["spans"].clone())?;
            if !refs.is_empty()
                && refs.iter().all(|part| {
                    members.iter().any(|member| {
                        member.line_id == part.line_id
                            && member.range.start <= part.range.start
                            && part.range.end <= member.range.end
                    })
                })
            {
                matches.push(*quotation);
            }
        }
        node["container_id"] = matches
            .into_iter()
            .min_by_key(|value| value["line_ids"].as_array().map_or(0, Vec::len))
            .map(|value| value["id"].clone())
            .unwrap_or(Value::Null);
    }
    Ok(ProjectedFacts {
        nodes,
        notes,
        fields,
        annotations: exported,
        documents,
        scopes,
        navigation,
    })
}

fn empty_grid_position(node: &mut Value, table: &Value) {
    node["range"] = json!({"start":table["range"]["start"],"end":table["range"]["start"]});
    node["document_id"] = table["document_id"].clone();
    node["page_indexes"] = table["page_indexes"].clone();
}

fn scope_fact(id: &str, kind: &str, ownership: Value, parent: Option<&str>) -> Value {
    json!({"id":id,"kind":kind,"ownership":ownership,"parent_scope_id":parent,"ranges":[]})
}

fn project_scopes(
    projection: &Projection<'_>,
    annotations: &[CompositionAnnotation],
    exported: &[Value],
) -> Result<(Vec<Value>, Vec<Value>)> {
    let docs: HashMap<_, _> = annotations
        .iter()
        .filter(|annotation| annotation.kind == "document")
        .map(|annotation| (annotation.id.as_str(), annotation))
        .collect();
    let parents: HashMap<_, _> = docs
        .iter()
        .map(|(id, annotation)| {
            (
                *id,
                annotation.attributes.get("parent").and_then(Value::as_str),
            )
        })
        .collect();
    let positions: HashMap<_, _> = projection
        .composition
        .reading_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let mut depths = HashMap::new();
    for id in docs.keys() {
        let mut current = Some(*id);
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if !seen.insert(id) {
                return Err("Document containment cycle".into());
            }
            current = *parents.get(id).ok_or("Absent parent document")?;
        }
        depths.insert(*id, seen.len() - 1);
    }
    let mut ordered: Vec<_> = docs.values().copied().collect();
    ordered.sort_by_key(|annotation| {
        (
            positions[annotation.spans[0].line_id.as_str()],
            annotation.spans[0].start,
            depths[annotation.id.as_str()],
            annotation.id.as_str(),
        )
    });
    let mut documents = Vec::new();
    let mut scopes = BTreeMap::<String, Value>::new();
    for annotation in ordered {
        let mut descendants = HashSet::from([annotation.id.as_str()]);
        loop {
            let more: Vec<_> = parents
                .iter()
                .filter(|(id, parent)| {
                    !descendants.contains(**id)
                        && parent.is_some_and(|parent| descendants.contains(parent))
                })
                .map(|(id, _)| *id)
                .collect();
            if more.is_empty() {
                break;
            }
            descendants.extend(more);
        }
        let members: Vec<_> = projection
            .composition
            .groups
            .iter()
            .filter(|group| {
                group
                    .ownership
                    .as_ref()
                    .is_some_and(|owner| descendants.contains(owner.document_id.as_str()))
            })
            .flat_map(|group| group.spans.iter().cloned())
            .collect();
        let own: Vec<_> = projection
            .composition
            .groups
            .iter()
            .filter(|group| {
                group
                    .ownership
                    .as_ref()
                    .is_some_and(|owner| owner.document_id == annotation.id)
            })
            .flat_map(|group| group.spans.iter().cloned())
            .collect();
        let precise = projection.project_spans(&members)?;
        let owned = projection.project_spans(&own)?;
        let mut value = projection.project(&encoded(annotation)?)?;
        value["parent_id"] = json!(parents[annotation.id.as_str()]);
        value["owned_spans"] = json!(owned);
        value["owned_line_ids"] = json!(unique(own.iter().map(|span| span.line_id.clone())));
        value["line_ids"] = json!(unique(members.iter().map(|span| span.line_id.clone())));
        value["ranges"] = json!(range_values(&precise));
        value["page_indexes"] = json!(precise
            .iter()
            .map(|part| part.page_index)
            .collect::<BTreeSet<_>>());
        let mut scope = scope_fact(
            &annotation.id,
            "document",
            json!({"document_id":annotation.id,"quotation_id":null,"reading_stream_id":null}),
            parents[annotation.id.as_str()],
        );
        scope["ranges"] = json!(range_values(&precise));
        scope["owned_ranges"] = json!(range_values(&owned));
        scope["title_ranges"] = json!(range_values(
            &projection.project_spans(&spans(annotation.attributes.get("title"))?)?
        ));
        scope["relationship"] = annotation
            .attributes
            .get("relationship")
            .cloned()
            .unwrap_or(Value::Null);
        scopes.insert(annotation.id.clone(), scope);
        documents.push(value);
    }
    for (annotation, value) in annotations
        .iter()
        .zip(exported)
        .filter(|(annotation, _)| annotation.kind == "quotation")
    {
        let document = value["document_id"]
            .as_str()
            .ok_or("Quotation has no document owner")?;
        let owner = annotation_owner(annotation);
        let parent = owner
            .get("quotation_id")
            .and_then(Value::as_str)
            .unwrap_or(document);
        let mut scope = scope_fact(
            &annotation.id,
            "quotation",
            json!({"document_id":document,"quotation_id":annotation.id,"reading_stream_id":null}),
            Some(parent),
        );
        scope["ranges"] = json!(range_values(&projection.project_spans(&annotation.spans)?));
        scope["layout"] = annotation
            .attributes
            .get("layout")
            .cloned()
            .unwrap_or(Value::Null);
        scope["attribution_ranges"] = json!(range_values(
            &projection.project_spans(&spans(annotation.attributes.get("attribution"))?)?
        ));
        scopes.insert(annotation.id.clone(), scope);
    }
    for scope in scopes.values_mut() {
        let ranges: Vec<ScalarRange> = decoded(scope["ranges"].clone())?;
        scope["ranges"] = json!(merged_ranges(ranges));
    }
    Ok((documents, scopes.into_values().collect()))
}

fn add_implicit_scopes(
    scopes: &mut BTreeMap<String, Value>,
    nodes: &[Value],
    document_id: &str,
) -> Result<()> {
    let declared: HashSet<_> = scopes.keys().cloned().collect();
    for node in nodes {
        if node["kind"] == "page" {
            continue;
        }
        let Some(owner) = node.get("ownership").and_then(Value::as_object) else {
            continue;
        };
        let document = owner
            .get("document_id")
            .and_then(Value::as_str)
            .ok_or("Node ownership has no document")?;
        let mut identities = Vec::new();
        if document != document_id {
            identities.push(("document", document, None));
        }
        if let Some(quotation) = owner.get("quotation_id").and_then(Value::as_str) {
            identities.push(("quotation", quotation, Some(document)));
        }
        if let Some(stream) = owner.get("reading_stream_id").and_then(Value::as_str) {
            identities.push(("reading_stream", stream, Some(document)));
        }
        for (kind, id, parent) in identities {
            if declared.contains(id) {
                continue;
            }
            let fact = scopes.entry(id.to_owned()).or_insert_with(||scope_fact(id,kind,json!({"document_id":document,
                "quotation_id":if kind=="quotation"{owner.get("quotation_id").cloned().unwrap_or(Value::Null)}else{Value::Null},
                "reading_stream_id":if kind=="reading_stream"{owner.get("reading_stream_id").cloned().unwrap_or(Value::Null)}else{Value::Null}}),parent));
            let ranges = node
                .get("ranges")
                .and_then(Value::as_array)
                .filter(|ranges| !ranges.is_empty())
                .cloned()
                .unwrap_or_else(|| vec![node["range"].clone()]);
            fact["ranges"].as_array_mut().unwrap().extend(ranges);
        }
    }
    Ok(())
}

fn project_navigation(
    projection: &Projection<'_>,
    witnesses: &[crate::NativeNavigationWitness],
) -> Result<Vec<Value>> {
    let entries: Vec<_> = projection
        .composition
        .annotations
        .iter()
        .filter(|annotation| annotation.kind == "navigation")
        .collect();
    let printed: HashMap<_, _> = entries
        .iter()
        .filter(|annotation| {
            annotation.attributes.get("kind").and_then(Value::as_str) == Some("contents")
        })
        .map(|annotation| (anchor(&annotation.spans[0]), annotation.id.as_str()))
        .collect();
    let confirmations: HashMap<_, _> = entries
        .iter()
        .filter(|annotation| {
            annotation.attributes.get("kind").and_then(Value::as_str) == Some("bookmark")
        })
        .filter_map(|annotation| {
            annotation
                .attributes
                .get("witness_id")
                .and_then(Value::as_str)
                .map(|id| (id, *annotation))
        })
        .collect();
    let entry = |id: &str,
                 kind: &str,
                 sources: &[SourceSpan],
                 targets: &[SourceSpan],
                 titles: &[SourceSpan],
                 parent: Option<String>,
                 witness: Option<&str>|
     -> Result<Value> {
        Ok(
            json!({"id":id,"kind":kind,"source_ranges":range_values(&projection.project_spans(sources)?),
            "source_line_ids":unique(sources.iter().map(|span|span.line_id.clone())),
            "title_ranges":range_values(&projection.project_spans(titles)?),"target_ranges":range_values(&projection.project_spans(targets)?),
            "target_line_ids":unique(targets.iter().map(|span|span.line_id.clone())),"parent_id":parent,"witness_id":witness}),
        )
    };
    let mut result = Vec::new();
    for annotation in entries.iter().filter(|annotation| {
        annotation.attributes.get("kind").and_then(Value::as_str) == Some("contents")
    }) {
        let parent = annotation
            .attributes
            .get("parent")
            .and_then(Value::as_str)
            .map(|id| {
                if projection.lines.contains_key(id) {
                    format!("{id}@0")
                } else {
                    id.to_owned()
                }
            });
        result.push(entry(
            &annotation.id,
            "contents",
            &annotation.spans,
            &spans(annotation.attributes.get("target"))?,
            &spans(annotation.attributes.get("title"))?,
            parent.and_then(|id| printed.get(&id).map(|id| (*id).to_owned())),
            None,
        )?);
    }
    for witness in witnesses {
        let targets = confirmations
            .get(witness.id.as_str())
            .map(|annotation| spans(annotation.attributes.get("target")))
            .transpose()?
            .unwrap_or_default();
        result.push(entry(
            &format!("bookmark:{}", witness.id),
            "bookmark",
            &[],
            &targets,
            &[],
            witness
                .parent_id
                .as_ref()
                .map(|id| format!("bookmark:{id}")),
            Some(&witness.id),
        )?);
    }
    Ok(result)
}

impl Projection<'_> {
    /// Native partial facts require unchanged-character witnesses; a whole source unit
    /// may follow its declared lineage through a transcription correction or split.
    fn map_parts(&self, range: ScalarRange, allow_whole: bool) -> Result<Vec<ProjectedSpan>> {
        if range.start > range.end {
            return Err("Reversed native range".into());
        }
        if range.start == range.end {
            let mut points = BTreeSet::new();
            for (id, original) in &self.originals {
                if !(original.range.start <= range.start && range.start <= original.range.end) {
                    continue;
                }
                let text = ScalarText::new(&original.text);
                let point = text
                    .scalar_at_utf16(range.start - original.range.start)
                    .ok_or("Native point splits a source character")?;
                for descendant in self.descendants.get(id).into_iter().flatten() {
                    let line = self.lines[descendant.as_str()];
                    let current = ScalarText::new(&line.text);
                    for run in self
                        .witnesses
                        .get(descendant)
                        .into_iter()
                        .flatten()
                        .filter(|run| run.source_start <= point && point <= run.source_end)
                    {
                        let at = run.start + point - run.source_start;
                        for member in self.rendered[descendant]
                            .iter()
                            .filter(|member| member.span.start <= at && at <= member.span.end)
                        {
                            let source_at = self.ranges[descendant].start + current.utf16(at);
                            let rendered_at = member.range.start
                                + self
                                    .logical_span(descendant, member.span.start, at)?
                                    .encode_utf16()
                                    .count();
                            points.insert((
                                descendant.clone(),
                                line.page - 1,
                                source_at,
                                rendered_at,
                            ));
                        }
                    }
                }
            }
            if points.len() != 1 {
                return Err("Native point lacks unique declared correspondence".into());
            }
            let (line_id, page_index, source_at, rendered_at) = points.into_iter().next().unwrap();
            return Ok(vec![ProjectedSpan {
                line_id,
                page_index,
                range: ScalarRange {
                    start: source_at,
                    end: source_at,
                },
                rendered_range: ScalarRange {
                    start: rendered_at,
                    end: rendered_at,
                },
            }]);
        }
        let mut originals: Vec<_> = self
            .originals
            .iter()
            .filter(|(_, original)| {
                original.range.start < range.end && range.start < original.range.end
            })
            .collect();
        originals.sort_by_key(|(_, original)| original.range.start);
        if originals.is_empty() {
            return Err("Native range has no retained source membership".into());
        }
        let mut result = Vec::new();
        for (id, original) in originals {
            let text = ScalarText::new(&original.text);
            let start = text
                .scalar_at_utf16(range.start.max(original.range.start) - original.range.start)
                .ok_or("Native range splits a source character")?;
            let end = text
                .scalar_at_utf16(range.end.min(original.range.end) - original.range.start)
                .ok_or("Native range splits a source character")?;
            if self
                .lines
                .get(id.as_str())
                .is_some_and(|line| line.text == original.text)
            {
                result.extend(self.project_span(&SourceSpan {
                    line_id: id.clone(),
                    start,
                    end,
                })?);
                continue;
            }
            let descendants = self
                .descendants
                .get(id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if allow_whole && start == 0 && end == text.len() && !descendants.is_empty() {
                for descendant in descendants {
                    let end = self.lines[descendant.as_str()].text.chars().count();
                    if end > 0 {
                        result.extend(self.project_span(&SourceSpan {
                            line_id: descendant.clone(),
                            start: 0,
                            end,
                        })?);
                    }
                }
                continue;
            }
            let mut covered = Vec::new();
            for descendant in descendants {
                for run in self.witnesses.get(descendant).into_iter().flatten() {
                    let lo = start.max(run.source_start);
                    let hi = end.min(run.source_end);
                    if lo >= hi {
                        continue;
                    }
                    covered.push((lo, hi));
                    result.extend(self.project_span(&SourceSpan {
                        line_id: descendant.clone(),
                        start: run.start + lo - run.source_start,
                        end: run.start + hi - run.source_start,
                    })?);
                }
            }
            covered.sort_unstable();
            let mut cursor = start;
            for (lo, hi) in covered {
                if lo > cursor {
                    break;
                }
                cursor = cursor.max(hi);
            }
            if cursor < end {
                return Err("Partial native range crosses unconfirmed transcription".into());
            }
        }
        Ok(result)
    }

    fn map_range(&self, range: ScalarRange) -> Result<ScalarRange> {
        let parts = merged_ranges(
            self.map_parts(range, true)?
                .into_iter()
                .map(|part| part.range)
                .collect(),
        );
        if parts.len() != 1 {
            return Err("Single native anchor projects to disjoint source membership".into());
        }
        Ok(parts[0])
    }

    fn remap_native(&self, value: &Value) -> Result<Value> {
        match value {
            Value::Object(object)
                if object.len() == 2
                    && object.contains_key("start")
                    && object.contains_key("end") =>
            {
                encoded(self.map_range(decoded(value.clone())?)?)
            }
            Value::Object(object) => object
                .iter()
                .map(|(key, value)| {
                    Ok((
                        key.clone(),
                        if key == "native_witness" {
                            value.clone() // Original evidence keeps its declared baseline coordinates.
                        } else {
                            self.remap_native(value)?
                        },
                    ))
                })
                .collect::<Result<Map<_, _>>>()
                .map(Value::Object),
            Value::Array(values) => values
                .iter()
                .map(|value| self.remap_native(value))
                .collect::<Result<Vec<_>>>()
                .map(Value::Array),
            _ => Ok(value.clone()),
        }
    }

    /// Native graph endpoints use source UTF-16 coordinates, including when the
    /// readable document has a separate manuscript projection.
    fn mapped_envelope(&self, range: ScalarRange) -> Result<ScalarRange> {
        let start = self
            .map_range(ScalarRange {
                start: range.start,
                end: range.start,
            })?
            .start;
        let end = self
            .map_range(ScalarRange {
                start: range.end,
                end: range.end,
            })?
            .start;
        let parts = self.map_parts(range, true)?;
        if start > end
            || parts.is_empty()
            || bounds(parts.iter().map(|part| part.range), start) != (ScalarRange { start, end })
        {
            return Err("Native envelope lacks unique ordered endpoint correspondence".into());
        }
        Ok(ScalarRange { start, end })
    }

    fn remap_cross_references(
        &self,
        value: &Value,
        text: &str,
        unconfirmed: &mut Vec<Value>,
    ) -> Result<Value> {
        let mut graph: crate::InstrumentCrossReferenceGraph = decoded(value.clone())?;
        let raw_edges = value["edges"]
            .as_array()
            .ok_or("Native graph edges missing")?;
        let coordinates = ScalarText::new(text);
        let mut edges = Vec::new();
        let mut evidence = Vec::new();
        for (original, raw) in graph.edges.iter().zip(raw_edges) {
            let mapped = (|| -> Result<(crate::InstrumentCrossReferenceEdge, Value)> {
                let mut edge = original.clone();
                let source = ScalarRange {
                    start: edge.source_start,
                    end: edge.source_end,
                };
                // Citation wording requires unchanged witnesses, even for a whole line.
                let parts = self.map_parts(source, false)?;
                if parts.is_empty()
                    || parts.windows(2).any(|pair| {
                        pair[0].range.end > pair[1].range.start
                            || !coordinates
                                .slice_utf16(pair[0].range.end..pair[1].range.start)
                                .is_some_and(|gap| gap.chars().all(char::is_whitespace))
                    })
                {
                    return Err("Native citation no longer has ordered contiguous wording".into());
                }
                let source = self.mapped_envelope(source)?;
                edge.source_start = source.start;
                edge.source_end = source.end;
                match (edge.target_start, edge.target_end) {
                    (Some(start), Some(end)) => {
                        let target = self.mapped_envelope(ScalarRange { start, end })?;
                        edge.target_start = Some(target.start);
                        edge.target_end = Some(target.end);
                    }
                    (None, None)
                        if edge.status != crate::InstrumentCrossReferenceStatus::Resolved => {}
                    _ => {
                        return Err("Native cross-reference target endpoints are incomplete".into())
                    }
                }
                let typed = encoded(&edge)?;
                let raw = self.remap_native(raw)?;
                Ok((edge, enriched_evidence(&raw, &typed).unwrap_or(typed)))
            })();
            match mapped {
                Ok((edge, raw)) => {
                    edges.push(edge);
                    evidence.push(raw);
                }
                Err(reason) => unconfirmed
                    .push(json!({"kind":"cross_reference","witness":raw,"reason":reason})),
            }
        }
        graph.edges = edges;
        let counts = &mut graph.counts;
        // Counts describe active mapped edges; rejected original edges remain in
        // unconfirmed_native_witnesses rather than inflating the active graph.
        counts.detected = graph.edges.len();
        counts.resolved = 0;
        counts.external = 0;
        counts.unresolved = 0;
        counts.abstained = 0;
        counts.self_loops = 0;
        for edge in &graph.edges {
            match edge.status {
                crate::InstrumentCrossReferenceStatus::Resolved => counts.resolved += 1,
                crate::InstrumentCrossReferenceStatus::External => counts.external += 1,
                crate::InstrumentCrossReferenceStatus::Unresolved => counts.unresolved += 1,
                crate::InstrumentCrossReferenceStatus::Abstained => counts.abstained += 1,
            }
            counts.self_loops += usize::from(edge.self_loop);
        }
        let accepted = counts.resolved + counts.unresolved;
        counts.integrity = if accepted == 0 {
            1.0
        } else {
            counts.resolved as f64 / accepted as f64
        };
        let typed = encoded(graph)?;
        let mut result = enriched_evidence(value, &typed).unwrap_or(typed);
        result["edges"] = json!(evidence);
        Ok(result)
    }

    fn remap_contents(&self, value: &Value) -> Result<Value> {
        let mut contents: crate::InstrumentContentsReading = decoded(value.clone())?;
        if let Some(outline) = &mut contents.outline {
            let region = self.mapped_envelope(ScalarRange {
                start: outline.region_start,
                end: outline.region_end,
            })?;
            outline.region_start = region.start;
            outline.region_end = region.end;
            for entry in &mut outline.entries {
                entry.contents_line_start = self
                    .map_range(ScalarRange {
                        start: entry.contents_line_start,
                        end: entry.contents_line_start,
                    })?
                    .start;
            }
        }
        let typed = encoded(contents)?;
        let raw = self.remap_native(value)?;
        Ok(enriched_evidence(&raw, &typed).unwrap_or(typed))
    }

    fn retained_node(&self, original: &Value) -> Result<Value> {
        let ranges: Vec<ScalarRange> = if let Some(ranges) = original
            .get("ranges")
            .and_then(Value::as_array)
            .filter(|ranges| !ranges.is_empty())
        {
            decoded(Value::Array(ranges.clone()))?
        } else {
            vec![decoded(original["range"].clone())?]
        };
        let mut parts = Vec::new();
        for range in ranges {
            parts.extend(self.map_parts(range, true)?);
        }
        let mut value = original.clone();
        let object = value
            .as_object_mut()
            .ok_or("Native node must be an object")?;
        for key in [
            "range",
            "ranges",
            "extent",
            "level",
            "rendered_range",
            "rendered_ranges",
            "rendered_membership",
            "content_start",
            "line_ids",
            "page_indexes",
        ] {
            object.remove(key);
        }
        let mut value = self.remap_native(&value)?;
        value["range"] = json!(bounds(parts.iter().map(|part| part.range), 0));
        value["ranges"] = json!(range_values(&parts));
        value["rendered_membership"] = json!(parts
            .iter()
            .map(|part| part.rendered_range)
            .collect::<Vec<_>>());
        value["line_ids"] = json!(unique(parts.iter().map(|part| part.line_id.clone())));
        value["page_indexes"] = json!(parts
            .iter()
            .map(|part| part.page_index)
            .collect::<BTreeSet<_>>());
        if let Some(point) = original
            .get("content_start")
            .filter(|point| !point.is_null())
        {
            let point: usize = decoded(point.clone())?;
            value["content_start"] = json!(
                self.map_range(ScalarRange {
                    start: point,
                    end: point
                })?
                .start
            );
        }
        Ok(value)
    }
}

/// Compose literal facts, project corrected coordinates, then derive the queryable graph.
pub fn project_structure_composition(
    request: CompositionProjectionRequest,
) -> Result<ProjectedStructureComposition> {
    if request.composition.through.is_some() || request.composition.source_correction {
        return Err("Document projection requires a complete structural composition".into());
    }
    let baseline = crate::composition_annotation_seed::typed_structure(&request.structure)?;
    if baseline.offset_unit != "utf16" {
        return Err("Original structure must use UTF-16 coordinates".into());
    }
    let original = request
        .structure
        .as_object()
        .ok_or("Original structure must be an object")?;
    let native_navigation = request.composition.native_navigation.clone();
    let structure_tags = request.composition.structure_tags.clone();
    let incoming_native: HashSet<_> = request
        .composition
        .removable_nodes
        .iter()
        .map(|node| node.id.clone())
        .collect();
    let mut parent_sources = BTreeMap::<String, Vec<SourceSpan>>::new();
    for group in &request.composition.groups {
        let members = if group.spans.is_empty() {
            group.line_ids.iter().map(|id| {
                let line = request.composition.lines.iter().find(|line| line.id == *id)
                    .ok_or("Native parent source line is absent")?;
                Ok(SourceSpan { line_id: id.clone(), start: 0, end: line.text.chars().count() })
            }).collect::<Result<Vec<_>>>()?
        } else { group.spans.clone() };
        for id in std::iter::once(group.id.as_str()).chain(group.attributes.get("native_id").and_then(Value::as_str)) {
            if !id.is_empty() { parent_sources.entry(id.to_owned()).or_default().extend(members.clone()); }
        }
    }
    let composition = apply_structure_composition(request.composition)?;
    let logical_ids: HashMap<_, _> = composition.logical_blocks.iter().flat_map(|(first, fragments)|
        fragments.iter().map(move |fragment| (fragment.as_str(), first.as_str()))).collect();
    let mut source_owners = HashMap::<&str, Vec<(usize, usize, &str)>>::new();
    for group in &composition.groups {
        for span in &group.spans {
            source_owners.entry(span.line_id.as_str()).or_default()
                .push((span.start, span.end, logical_ids[group.id.as_str()]));
        }
    }
    for owners in source_owners.values_mut() { owners.sort_by_key(|&(start, end, _)| (start, end)); }
    let parent_replacements = parent_sources.into_iter().map(|(id, members)| {
        let mut replacements = BTreeSet::new();
        for member in members {
            if let Some(owners) = source_owners.get(member.line_id.as_str()) {
                let from = owners.partition_point(|&(_, end, _)| end <= member.start);
                for &(_, _, logical) in owners[from..].iter().take_while(|&&(start, _, _)| start < member.end) {
                    replacements.insert(logical.to_owned());
                }
            }
        }
        (id, replacements)
    }).collect::<BTreeMap<_, _>>();
    let surviving_native: HashSet<_> = composition
        .removable_nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    let (projection, text, manuscript, mut pages) = Projection::new(&composition, &baseline)?;
    let mut facts = project_facts(&projection, &baseline, &native_navigation)?;
    let original_nodes: HashMap<_, _> = original
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("Original nodes missing")?
        .iter()
        .map(|node| {
            node.get("id")
                .and_then(Value::as_str)
                .map(|id| (id, node))
                .ok_or_else(|| "Original node identity missing".to_owned())
        })
        .collect::<Result<_>>()?;
    if original_nodes.len() != original["nodes"].as_array().unwrap().len() {
        return Err("Repeated original node identity".into());
    }
    let mut selected = HashSet::new();
    for id in &request.retained_node_ids {
        if !selected.insert(id.as_str()) || !original_nodes.contains_key(id.as_str()) {
            return Err("Retained native selection names an absent or repeated node".into());
        }
    }
    let generated: HashSet<_> = facts
        .nodes
        .iter()
        .chain(&pages)
        .filter_map(|node| node["id"].as_str())
        .collect();
    let mut retained = Vec::new();
    let mut unconfirmed = original
        .get("unconfirmed_native_witnesses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for id in &request.retained_node_ids {
        if incoming_native.contains(id) && !surviving_native.contains(id.as_str()) {
            continue;
        }
        if generated.contains(id.as_str()) {
            continue;
        }
        let node = original_nodes[id.as_str()];
        if matches!(
            node["kind"].as_str(),
            Some("document" | "quotation" | "note")
        ) {
            unconfirmed.push(json!({"kind":"node","witness":node,
                "reason":"Raw annotation node has no represented composition annotation"}));
            continue;
        }
        match projection.retained_node(node) {
            Ok(node) => retained.push(node),
            Err(reason) => unconfirmed.push(json!({"kind":"node","witness":node,"reason":reason})),
        }
    }
    let retained_ids: HashSet<_> = generated
        .into_iter()
        .chain(retained.iter().filter_map(|node| node["id"].as_str()))
        .map(str::to_owned)
        .collect();
    for node in facts.nodes.iter_mut().chain(&mut pages).chain(&mut retained) {
        if node.get("ownership").is_none_or(Value::is_null) {
            if let Some(document) = node.get("document_id").and_then(Value::as_str) {
                node["ownership"] =
                    json!({"document_id":document,"quotation_id":null,"reading_stream_id":null});
            }
        }
    }
    let parent_context: HashMap<_, _> = facts.nodes.iter().chain(&pages).chain(&retained)
        .filter_map(|node| node["id"].as_str().map(|id| {
            decoded::<crate::NodeKind>(node["kind"].clone())
                .map(|kind| (id.to_owned(), (node["ownership"].clone(), kind)))
        })).collect::<Result<_>>()?;
    let unchanged_parents: HashSet<_> = retained.iter().filter_map(|node| node["id"].as_str().map(str::to_owned)).collect();
    for node in &mut retained {
        let Some(parent) = node["parent_id"].as_str().map(str::to_owned) else { continue; };
        let replacements = parent_replacements.get(&parent);
        let replacement = if unchanged_parents.contains(&parent) || replacements.is_none() && retained_ids.contains(&parent) {
            Some(&parent)
        } else { replacements.filter(|ids| ids.len() == 1).and_then(|ids| ids.first()) };
        let kind: crate::NodeKind = decoded(node["kind"].clone())?;
        if let Some(replacement) = replacement.filter(|id| parent_context.get(*id).is_none_or(|(owner, parent_kind)|
            owner.is_null() || node["ownership"].is_null() || owner == &node["ownership"] || kind.is_grid_child_of(*parent_kind))) {
            node["parent_id"] = json!(replacement);
        } else {
            let reason = if replacement.is_some() {
                "Native parent relationship crosses current source ownership"
            } else if replacements.is_some_and(|ids| ids.len() > 1) {
                "Native parent has multiple distinct logical replacements"
            } else { "Native parent has no surviving source identity" };
            let evidence = json!({"kind":"structural_parent","reason":reason,"witness":{
                "document_id":baseline.document_id,"text_sha256":baseline.text_sha256,
                "source_sha256":baseline.source_sha256,"offset_unit":baseline.offset_unit,
                "facts":{"node_id":node["id"],"parent_id":parent}}});
            if !unconfirmed.contains(&evidence) { unconfirmed.push(evidence); }
            node["parent_id"] = Value::Null;
        }
    }
    // Page nodes retain their producer-only evidence, with current ranges replacing old coordinates.
    for page in &mut pages {
        let id = page["id"].as_str().ok_or("Page identity absent")?;
        let mut evidence = original_nodes[id].as_object().unwrap().clone();
        for key in [
            "ranges",
            "extent",
            "heading_range",
            "level",
            "content_start",
            "marker_range",
            "rendered_range",
            "rendered_ranges",
            "rendered_membership",
        ] {
            evidence.remove(key);
        }
        evidence.extend(page.as_object().unwrap().clone());
        *page = Value::Object(evidence);
    }
    let mut metadata = Map::new();
    for (key, value) in original {
        if matches!(
            key.as_str(),
            "unconfirmed_native_witnesses"
                | "text"
                | "nodes"
                | "notes"
                | "source_fragments"
                | "structure_tags"
                | "fields"
                | "annotations"
                | "documents"
                | "navigation"
                | "native_navigation"
                | "continuations"
                | "joins"
                | "manuscript_edits"
                | "rendered_text"
                | "scope_annotations"
        ) {
            continue;
        }
        let mapped = match key.as_str() {
            "native_witness" => Ok(value.clone()),
            "cross_references" if !value.is_null() => {
                projection.remap_cross_references(value, &text, &mut unconfirmed)
            }
            "contents" if !value.is_null() => projection.remap_contents(value),
            _ => projection.remap_native(value),
        };
        match mapped {
            Ok(value) => {
                metadata.insert(key.clone(), value);
            }
            Err(reason) => {
                unconfirmed
                    .push(json!({"kind":"metadata","field":key,"witness":value,"reason":reason}));
            }
        }
    }
    let mut stream_witnesses = metadata.get("native_witness").and_then(Value::as_array).cloned().unwrap_or_default();
    crate::composition_native::preserve_reading_stream_evidence(&request.structure,
        &projection.originals.values().map(|line| line.range).collect::<Vec<_>>(),
        &mut stream_witnesses, &mut unconfirmed)?;
    if !stream_witnesses.is_empty() { metadata.insert("native_witness".into(), json!(stream_witnesses)); }
    metadata.entry("diagnostics").or_insert_with(|| json!([]));
    let mut tags = Vec::new();
    for original_tag in &structure_tags {
        let mut tag = original_tag.clone();
        tag.ranges.clear();
        tag.line_ids.clear();
        let mapped = original_tag
            .ranges
            .iter()
            .map(|range| projection.map_parts(*range, false))
            .collect::<Result<Vec<_>>>();
        match mapped {
            Ok(parts) => {
                let parts: Vec<_> = parts.into_iter().flatten().collect();
                tag.ranges = range_values(&parts);
                tag.line_ids = unique(parts.into_iter().map(|part| part.line_id));
            }
            Err(reason) => unconfirmed
                .push(json!({"kind":"structure_tag","witness":original_tag,"reason":reason})),
        }
        tags.push(tag);
    }
    let native_node_ids: Vec<String> = retained
        .iter()
        .filter_map(|node| node["id"].as_str().map(str::to_owned))
        .collect();
    let mut nodes = pages;
    nodes.append(&mut facts.nodes);
    nodes.extend(retained);
    let mut scopes: BTreeMap<String, Value> = facts
        .scopes
        .into_iter()
        .map(|scope| (scope["id"].as_str().unwrap().to_owned(), scope))
        .collect();
    add_implicit_scopes(&mut scopes, &nodes, &baseline.document_id)?;
    for scope in scopes.values_mut() {
        scope["ranges"] = json!(merged_ranges(decoded(scope["ranges"].clone())?));
    }
    let mut continuations = Vec::new();
    for (kind, layer) in &composition.continuations {
        for (first, link) in layer {
            continuations.push(json!({"kind":kind,"from":projection.anchor(first)?,"to":projection.anchor(&link.next)?,"separator":link.separator}));
        }
    }
    let mut joins = Vec::new();
    for (first, link) in &composition.joins {
        joins.push(json!({"from":projection.anchor(first)?,"to":projection.anchor(&link.next)?,"separator":link.separator}));
    }
    let fragments: Vec<_> = composition.lines.iter().map(|line|json!({"id":line.id,"page_index":line.page-1,"range":projection.ranges[&line.id]})).collect();
    metadata.extend(json!({"text":text,"text_sha256":format!("{:x}",Sha256::digest(text.as_bytes())),"rendered_text":manuscript,
        "nodes":nodes,"notes":facts.notes,"fields":facts.fields,"annotations":facts.annotations,"documents":facts.documents,
        "source_fragments":fragments,"scope_annotations":scopes.into_values().collect::<Vec<_>>(),"structure_tags":tags,
        "navigation":facts.navigation,"native_navigation":native_navigation,"continuations":continuations,"joins":joins,
        "manuscript_edits":composition.manuscript_edits}).as_object().unwrap().clone());
    metadata.insert("unconfirmed_native_witnesses".into(), json!(unconfirmed));
    let native_facts =
        crate::capture_composition_native_facts(crate::CompositionNativeCaptureRequest {
            structure: Value::Object(metadata.clone()),
            lines: Vec::new(),
            retained_node_ids: native_node_ids,
        })?;
    let mut structure: DocumentStructure = decoded(Value::Object(metadata.clone()))?;
    structure = assemble_structure_annotations(structure)?.structure;
    let serialized = encoded(&structure)?;
    let structure_evidence = extra_evidence(&metadata, serialized.as_object().unwrap())?;
    let mut source_trace = Vec::new();
    for line in &composition.lines {
        let mut value = encoded(line)?;
        let rendered = &projection.rendered[&line.id];
        value["range"] = json!(projection.ranges[&line.id]);
        value["rendered_range"] = json!(bounds(rendered.iter().map(|part| part.range), 0));
        value["rendered_spans"] = json!(rendered);
        source_trace.push(value);
    }
    Ok(ProjectedStructureComposition {
        native_facts,
        structure,
        composition,
        source_trace,
        unconfirmed_native_witnesses: unconfirmed,
        structure_evidence,
    })
}

/// Return canonical typed values enriched only with evidence leaves absent from
/// the typed export. Whole nested containers can then be overlaid without a
/// second coordinate or representation policy in an artifact adapter.
fn enriched_evidence(raw: &Value, typed: &Value) -> Option<Value> {
    match (raw, typed) {
        (Value::Object(raw), Value::Object(typed)) => {
            let mut merged = typed.clone();
            let mut changed = false;
            for (key, value) in raw {
                if let Some(canonical) = typed.get(key) {
                    if let Some(enriched) = enriched_evidence(value, canonical) {
                        merged.insert(key.clone(), enriched);
                        changed = true;
                    }
                } else {
                    merged.insert(key.clone(), value.clone());
                    changed = true;
                }
            }
            changed.then_some(Value::Object(merged))
        }
        (Value::Array(raw), Value::Array(typed)) if raw.len() == typed.len() => {
            let mut changed = false;
            let merged = raw
                .iter()
                .zip(typed)
                .map(|(raw, typed)| {
                    if let Some(enriched) = enriched_evidence(raw, typed) {
                        changed = true;
                        enriched
                    } else {
                        typed.clone()
                    }
                })
                .collect();
            changed.then_some(Value::Array(merged))
        }
        _ => None,
    }
}

fn evidence_fields(
    raw: &Map<String, Value>,
    typed: &Map<String, Value>,
    excluded: &[&str],
) -> Map<String, Value> {
    let mut evidence = Map::new();
    for (key, value) in raw {
        if excluded.contains(&key.as_str()) {
            continue;
        }
        match typed.get(key) {
            Some(canonical) => {
                if let Some(enriched) = enriched_evidence(value, canonical) {
                    evidence.insert(key.clone(), enriched);
                }
            }
            None => {
                evidence.insert(key.clone(), value.clone());
            }
        }
    }
    evidence
}

fn extra_evidence(
    raw: &Map<String, Value>,
    typed: &Map<String, Value>,
) -> Result<CompositionStructureEvidence> {
    let mut result = CompositionStructureEvidence {
        fields: evidence_fields(raw, typed, &["nodes", "notes"]),
        ..Default::default()
    };
    for (field, output) in [("nodes", &mut result.nodes), ("notes", &mut result.notes)] {
        let canonical: HashMap<_, _> = typed
            .get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("id").and_then(Value::as_str).map(|id| (id, item)))
            .collect();
        for item in raw
            .get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .ok_or("Projected evidence identity absent")?;
            let known = canonical
                .get(id)
                .and_then(|value| value.as_object())
                .ok_or("Projected evidence lacks typed owner")?;
            let evidence = evidence_fields(
                item.as_object()
                    .ok_or("Projected evidence must be an object")?,
                known,
                &[],
            );
            if !evidence.is_empty() {
                output.insert(id.to_owned(), evidence);
            }
        }
    }
    Ok(result)
}
