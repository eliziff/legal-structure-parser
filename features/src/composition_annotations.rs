//! Annotations as logical objects: a note or table continued across fragments read as one.
use crate::composition::{anchor, spans, Source};
use crate::{CompositionAnnotation, CompositionLine, CompositionLinks};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Deserialize)]
pub struct CompositionAnnotationsRequest {
    pub lines: Vec<CompositionLine>,
    pub reading_order: Vec<String>,
    pub annotations: Vec<CompositionAnnotation>,
    #[serde(default)]
    pub continuations: CompositionLinks,
}

#[derive(Debug, Serialize)]
pub struct ResolvedCompositionAnnotations {
    pub annotations: Vec<CompositionAnnotation>,
}

/// Each annotation with the fragments its note or table continuations join to it.
pub fn resolve_composition_annotations(mut request: CompositionAnnotationsRequest) -> Result<ResolvedCompositionAnnotations> {
    let source = Source::new(&request.lines, &request.reading_order)?;
    for annotation in &mut request.annotations {
        source.membership(&mut annotation.line_ids, &mut annotation.spans)?;
    }
    let annotations = &request.annotations;
    // The annotation of a kind that opens at an anchor, or else holds it.
    let object = |kind: &str, id: &str| -> Result<Option<&CompositionAnnotation>> {
        if let Some(found) = annotations.iter().find(|a| a.kind == kind && anchor(&a.spans[0]) == id) {
            return Ok(Some(found));
        }
        let (position, offset) = source.location(id)?;
        Ok(annotations.iter().find(|a| a.kind == kind && a.spans.iter().any(|s|
            source.positions[s.line_id.as_str()] == position && s.start <= offset && offset < s.end)))
    };
    let mut next_of = HashMap::<String, String>::new();
    for kind in ["note", "table"] {
        for (first, link) in request.continuations.get(kind).into_iter().flatten() {
            let left = object(kind, &source.canonical(first))?.ok_or("Continuation names absent annotation fragment")?;
            let right = object(kind, &source.canonical(&link.next))?.ok_or("Continuation names absent annotation fragment")?;
            if left.id != right.id {
                next_of.insert(left.id.clone(), right.id.clone());
            }
        }
    }
    let mut ordered = annotations.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|a| (source.positions[a.spans[0].line_id.as_str()], a.spans[0].start));
    let by_id = ordered.iter().map(|a| (a.id.as_str(), *a)).collect::<HashMap<_, _>>();
    let mut consumed = HashSet::new();
    let mut result = Vec::new();
    for annotation in ordered {
        if consumed.contains(&annotation.id) { continue; }
        let mut parts = vec![annotation];
        consumed.insert(annotation.id.clone());
        // A part two chains claim (two notes continuing into one part, or a chain that turns back) stays
        // with the chain that reached it first in reading order; the other chain ends there rather than
        // failing the document.
        while let Some(next) = next_of.get(&parts.last().unwrap().id).and_then(|id| by_id.get(id.as_str()).copied()) {
            if !consumed.insert(next.id.clone()) { break; }
            parts.push(next);
        }
        let mut merged = annotation.clone();
        let mut seen = HashSet::new();
        merged.line_ids = parts.iter().flat_map(|part| part.line_ids.iter().cloned()).filter(|id| seen.insert(id.clone())).collect();
        merged.spans = parts.iter().flat_map(|part| part.spans.iter().cloned()).collect();
        if annotation.kind == "note" {
            for field in ["references", "label"] {
                let members = parts.iter().map(|part| spans(part.attributes.get(field))).collect::<Result<Vec<_>>>()?;
                merged.attributes.insert(field.into(), json!(members.concat()));
            }
        } else if annotation.kind == "table" {
            let (mut rows, mut merges, mut headers) = (Vec::<Value>::new(), Vec::<[usize; 4]>::new(), Vec::<usize>::new());
            for part in &parts {
                let attribute = |key: &str| part.attributes.get(key).cloned().unwrap_or_else(|| json!([]));
                let next: Vec<Value> = serde_json::from_value(attribute("rows")).map_err(|e| e.to_string())?;
                let offset = rows.len();
                let part_merges: Vec<[usize; 4]> = serde_json::from_value(attribute("merges")).map_err(|e| e.to_string())?;
                merges.extend(part_merges.into_iter().map(|[row, column, height, width]| [row + offset, column, height, width]));
                let part_headers: Vec<usize> = serde_json::from_value(attribute("header_rows")).map_err(|e| e.to_string())?;
                headers.extend(part_headers.into_iter().map(|row| row + offset));
                rows.extend(next);
            }
            merged.attributes = json!({"rows": rows, "merges": merges, "header_rows": headers}).as_object().unwrap().clone();
        }
        result.push(merged);
    }
    Ok(ResolvedCompositionAnnotations { annotations: result })
}
