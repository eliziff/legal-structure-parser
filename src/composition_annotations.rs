//! Source annotation identities and views, independent of manuscript assembly.
use crate::composition::{canonical_ref, location, Source};
use crate::{
    AppliedComposition, CompositionAnnotation, CompositionBlock, CompositionLine, CompositionLink,
    SourceSpan,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionAnnotationsRequest {
    pub lines: Vec<CompositionLine>,
    pub reading_order: Vec<String>,
    pub annotations: Vec<CompositionAnnotation>,
    pub continuations: BTreeMap<String, BTreeMap<String, CompositionLink>>,
    #[serde(default)]
    pub groups: Vec<CompositionBlock>,
    #[serde(default)]
    pub through: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ResolvedCompositionAnnotations {
    pub annotations: Vec<CompositionAnnotation>,
    /// Links whose target lies beyond the observed prefix, keyed by family and source anchor.
    pub pending_continuations: BTreeMap<String, BTreeMap<String, CompositionLink>>,
    pub quotation_scopes: BTreeMap<String, Option<String>>,
}

fn source<'a>(lines: &'a [CompositionLine], order: &'a [String]) -> Source<'a> {
    Source {
        lines: lines.iter().map(|line| (line.id.as_str(), line)).collect(),
        positions: order
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect(),
        targets: HashSet::new(),
        visible: HashSet::new(),
        metadata: &[],
        navigation: &[],
    }
}
fn anchor(span: &SourceSpan) -> String {
    format!("{}@{}", span.line_id, span.start)
}
fn spans(value: Option<&Value>) -> Result<Vec<SourceSpan>> {
    serde_json::from_value(value.cloned().unwrap_or_else(|| json!([])))
        .map_err(|error| error.to_string())
}
fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

/// Read recorded annotation fragments without requiring block hierarchy or text separators.
pub fn resolve_composition_annotations(
    mut request: CompositionAnnotationsRequest,
) -> Result<ResolvedCompositionAnnotations> {
    let source = source(&request.lines, &request.reading_order);
    if source.lines.len() != request.lines.len()
        || source.positions.len() != request.reading_order.len()
        || source.lines.len() != source.positions.len()
        || source
            .lines
            .keys()
            .any(|id| !source.positions.contains_key(id))
        || request
            .lines
            .iter()
            .any(|line| line.id.is_empty() || line.page == 0)
    {
        return Err(
            "Annotation view requires unique source identities and complete reading order".into(),
        );
    }
    let mut identities = HashSet::new();
    for annotation in &mut request.annotations {
        source.membership(&mut annotation.line_ids, &mut annotation.spans)?;
        if !matches!(
            annotation.kind.as_str(),
            "note" | "table" | "quotation" | "field" | "document" | "navigation"
        ) {
            return Err("Unknown structural annotation family".into());
        }
        if annotation.id.is_empty() {
            return Err("Annotation identity missing".into());
        }
        // This is a view over carried proposals, not the final annotation gate.
        // Validate the source references we consume, without judging an unfinished
        // note label/reference pairing or requiring a completed table classifier.
        let fields: &[&str] = match annotation.kind.as_str() {
            "note" => &["label", "references"],
            "quotation" => &["attribution"],
            _ => &[],
        };
        for field in fields {
            source.check(&spans(annotation.attributes.get(*field))?, false)?;
        }
        if !identities.insert(annotation.id.clone()) {
            return Err("Repeated annotation identity".into());
        }
    }
    let mut group_ids = HashSet::new();
    for group in &mut request.groups {
        source.membership(&mut group.line_ids, &mut group.spans)?;
        if group.id.is_empty() {
            group.id = anchor(&group.spans[0]);
        }
        if !group_ids.insert(group.id.clone()) {
            return Err("Repeated block identity".into());
        }
        source.check(&spans(group.attributes.get("marker"))?, false)?;
    }
    resolve(
        &source,
        &request.annotations,
        &request.groups,
        &request.continuations,
        request.through,
    )
}

pub(crate) fn resolve_applied_annotations(
    composition: &AppliedComposition,
) -> Result<ResolvedCompositionAnnotations> {
    resolve(
        &source(&composition.lines, &composition.reading_order),
        &composition.annotations,
        &composition.groups,
        &composition.continuations,
        None,
    )
}

/// One containment policy for composer ownership and source-only annotation views.
pub(crate) fn quotation_scope<'a>(
    members: &[SourceSpan],
    quotations: &'a [(String, Vec<SourceSpan>)],
    exclude: Option<&str>,
) -> Option<&'a str> {
    if members.is_empty() {
        return None;
    }
    quotations
        .iter()
        .filter(|(id, spans)| {
            Some(id.as_str()) != exclude
                && members.iter().all(|member| {
                    spans.iter().any(|span| {
                        span.line_id == member.line_id
                            && span.start <= member.start
                            && member.end <= span.end
                    })
                })
        })
        .min_by_key(|(_, spans)| {
            spans
                .iter()
                .map(|span| span.end - span.start)
                .sum::<usize>()
        })
        .map(|(id, _)| id.as_str())
}

fn resolve(
    source: &Source<'_>,
    annotations: &[CompositionAnnotation],
    groups: &[CompositionBlock],
    continuations: &BTreeMap<String, BTreeMap<String, CompositionLink>>,
    through: Option<usize>,
) -> Result<ResolvedCompositionAnnotations> {
    let observed = |id: &str| -> Result<bool> {
        let (position, _) = location(id, source)?;
        Ok(through.is_none_or(|last| {
            source
                .lines
                .values()
                .any(|line| source.positions[line.id.as_str()] == position && line.page <= last)
        }))
    };
    let object = |kind: &str, id: &str| -> Result<Option<&CompositionAnnotation>> {
        if let Some(annotation) = annotations
            .iter()
            .find(|annotation| annotation.kind == kind && anchor(&annotation.spans[0]) == id)
        {
            return Ok(Some(annotation));
        }
        let (position, offset) = location(id, source)?;
        let mut matches = annotations.iter().filter(|annotation| {
            annotation.kind == kind
                && annotation.spans.iter().any(|span| {
                    source.positions[span.line_id.as_str()] == position
                        && span.start <= offset
                        && offset < span.end
                })
        });
        let found = matches.next();
        if matches.next().is_some() {
            return Err("Continuation anchor names ambiguous annotation fragments".into());
        }
        Ok(found)
    };
    let mut object_links = HashMap::<String, String>::new();
    let mut object_incoming = HashMap::<String, String>::new();
    let mut pending = BTreeMap::<String, BTreeMap<String, CompositionLink>>::new();
    for (kind, links) in continuations {
        if !matches!(
            kind.as_str(),
            "paragraph" | "heading" | "note" | "table" | "quotation" | "list"
        ) {
            return Err("Unknown continuation kind".into());
        }
        if !matches!(kind.as_str(), "note" | "table" | "quotation") {
            continue;
        }
        let mut normalized = BTreeMap::new();
        let mut incoming = HashSet::new();
        for (first, link) in links {
            let first = canonical_ref(first, source);
            let next = canonical_ref(&link.next, source);
            if location(&first, source)? >= location(&next, source)?
                || !incoming.insert(next.clone())
                || normalized.contains_key(&first)
            {
                return Err("Annotation continuation is backwards, duplicated or branches".into());
            }
            if kind != "note" && link.separator.is_some()
                || link
                    .separator
                    .as_ref()
                    .is_some_and(|value| !value.chars().all(char::is_whitespace))
            {
                return Err("Invalid annotation continuation separator".into());
            }
            let normalized_link = CompositionLink {
                next: next.clone(),
                separator: link.separator.clone(),
                attributes: link.attributes.clone(),
            };
            if observed(&first)? {
                let left =
                    object(kind, &first)?.ok_or("Continuation names absent annotation fragment")?;
                if !observed(&next)? {
                    pending
                        .entry(kind.clone())
                        .or_default()
                        .insert(first.clone(), normalized_link.clone());
                } else {
                    let right = object(kind, &next)?
                        .ok_or("Continuation names absent annotation fragment")?;
                    if left.id != right.id {
                        if next != anchor(&right.spans[0]) {
                            return Err(
                                "Continuation must enter another annotation at its opening".into(),
                            );
                        }
                        if object_links
                            .insert(left.id.clone(), right.id.clone())
                            .is_some_and(|prior| prior != right.id)
                            || object_incoming
                                .insert(right.id.clone(), left.id.clone())
                                .is_some_and(|prior| prior != left.id)
                        {
                            return Err(
                                "Annotation continuation branches between logical objects".into()
                            );
                        }
                    }
                }
            }
            normalized.insert(first, normalized_link);
        }
    }
    let mut ordered: Vec<_> = annotations
        .iter()
        .filter(|annotation| {
            through
                .is_none_or(|last| source.lines[annotation.spans[0].line_id.as_str()].page <= last)
        })
        .collect();
    ordered.sort_by_key(|annotation| {
        (
            source.positions[annotation.spans[0].line_id.as_str()],
            annotation.spans[0].start,
        )
    });
    let by_id: HashMap<_, _> = ordered
        .iter()
        .map(|annotation| (annotation.id.as_str(), *annotation))
        .collect();
    let mut consumed = HashSet::new();
    let mut result = Vec::new();
    for annotation in ordered {
        if consumed.contains(&annotation.id) {
            continue;
        }
        let mut parts = vec![annotation];
        let mut current = annotation;
        let mut seen = HashSet::from([annotation.id.clone()]);
        while let Some(next_id) = object_links.get(&current.id) {
            let next = by_id
                .get(next_id.as_str())
                .ok_or("Observed continuation target is absent")?;
            if !seen.insert(next.id.clone()) {
                return Err("Annotation continuation cycle".into());
            }
            consumed.insert(next.id.clone());
            parts.push(*next);
            current = next;
        }
        let mut merged = annotation.clone();
        merged.line_ids = unique(parts.iter().flat_map(|part| part.line_ids.iter().cloned()));
        merged.spans = parts
            .iter()
            .flat_map(|part| part.spans.iter().cloned())
            .collect();
        let mut witnesses = Vec::new();
        for part in &parts {
            if let Some(value) = part.evidence.get("native_witness") {
                for witness in value
                    .as_array()
                    .ok_or("Native annotation witnesses must be an array")?
                {
                    if !witnesses.contains(witness) {
                        witnesses.push(witness.clone());
                    }
                }
            }
        }
        if !witnesses.is_empty() {
            merged
                .evidence
                .insert("native_witness".into(), Value::Array(witnesses));
        }
        let mut fragments = Vec::new();
        for part in &parts {
            if let Some(ids) = part.evidence.get("fragment_ids") {
                fragments.extend(
                    serde_json::from_value::<Vec<String>>(ids.clone())
                        .map_err(|error| error.to_string())?,
                );
            } else {
                fragments.push(part.id.clone());
            }
        }
        merged
            .evidence
            .insert("fragment_ids".into(), json!(fragments));
        if matches!(annotation.kind.as_str(), "note" | "quotation") {
            let fields: &[&str] = if annotation.kind == "note" {
                &["references", "label"]
            } else {
                &["attribution"]
            };
            for field in fields {
                let mut members = Vec::new();
                for part in &parts {
                    members.extend(spans(part.attributes.get(*field))?);
                }
                merged.attributes.insert((*field).into(), json!(members));
            }
        } else if annotation.kind == "table" {
            let mut rows = Vec::<Value>::new();
            let mut merges = Vec::<[usize; 4]>::new();
            let mut headers = Vec::<usize>::new();
            for part in &parts {
                let next: Vec<Value> = serde_json::from_value(
                    part.attributes
                        .get("rows")
                        .cloned()
                        .ok_or("Table view requires rows")?,
                )
                .map_err(|error| error.to_string())?;
                if let (Some(first), Some(next)) = (rows.first(), next.first()) {
                    if first.as_array().map(Vec::len) != next.as_array().map(Vec::len) {
                        return Err("Continued table column counts differ".into());
                    }
                }
                let offset = rows.len();
                for [row, column, height, width] in serde_json::from_value::<Vec<[usize; 4]>>(
                    part.attributes
                        .get("merges")
                        .cloned()
                        .unwrap_or_else(|| json!([])),
                )
                .map_err(|error| error.to_string())?
                {
                    merges.push([
                        row.checked_add(offset).ok_or("Table row offset overflow")?,
                        column,
                        height,
                        width,
                    ]);
                }
                headers.extend(
                    serde_json::from_value::<Vec<usize>>(
                        part.attributes
                            .get("header_rows")
                            .cloned()
                            .unwrap_or_else(|| json!([])),
                    )
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|row| {
                        row.checked_add(offset)
                            .ok_or_else(|| "Table header offset overflow".to_owned())
                    })
                    .collect::<Result<Vec<_>>>()?,
                );
                rows.extend(next);
            }
            merged.attributes = json!({"rows":rows,"merges":merges,"header_rows":headers})
                .as_object()
                .unwrap()
                .clone();
        }
        result.push(merged);
    }
    let quotes: Vec<_> = result
        .iter()
        .filter(|annotation| annotation.kind == "quotation")
        .map(|annotation| (annotation.id.clone(), annotation.spans.clone()))
        .collect();
    let mut quotation_scopes = BTreeMap::new();
    for group in groups {
        if through.is_some_and(|last| source.lines[group.spans[0].line_id.as_str()].page > last) {
            continue;
        }
        let markers = spans(group.attributes.get("marker"))?;
        let members = if markers.is_empty() {
            &group.spans
        } else {
            &markers
        };
        quotation_scopes.insert(
            group.id.clone(),
            quotation_scope(members, &quotes, None).map(str::to_owned),
        );
    }
    Ok(ResolvedCompositionAnnotations {
        annotations: result,
        pending_continuations: pending,
        quotation_scopes,
    })
}
