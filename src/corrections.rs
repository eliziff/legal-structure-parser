//! Literal source corrections in Unicode scalar coordinates. No inference runs here.
use crate::ScalarText;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTextEdit {
    pub line_id: String,
    pub start: usize,
    pub end: usize,
    pub old: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionAnchorKind {
    #[default]
    Annotation,
    Evidence,
    Normalization,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub line_id: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionAnchor {
    pub span: SourceSpan,
    #[serde(default)]
    pub kind: CorrectionAnchorKind,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCorrectionRequest {
    pub lines: BTreeMap<String, String>,
    pub edits: Vec<SourceTextEdit>,
    pub anchors: Vec<CorrectionAnchor>,
    /// Native removal addressability may become unknown after transcription.
    #[serde(default)]
    pub native_anchors: Vec<SourceSpan>,
    #[serde(default)]
    pub witnesses: BTreeMap<String, Vec<WitnessMapping>>,
}

#[derive(Debug, Serialize)]
pub struct SourceCorrectionResult {
    pub lines: BTreeMap<String, String>,
    /// Same order as the supplied anchors; identities are never reduced to line IDs.
    pub anchors: Vec<SourceSpan>,
    /// Input order is preserved; None means the former boundary is no longer exact.
    pub native_anchors: Vec<Option<SourceSpan>>,
    pub witnesses: BTreeMap<String, Vec<WitnessMapping>>,
}

/// All edits address the unchanged input. Failure returns no partially edited state.
pub fn correct_source_text(
    request: SourceCorrectionRequest,
) -> Result<SourceCorrectionResult, String> {
    if request.edits.is_empty() {
        return Err("Transcription needs exact edits".into());
    }
    let mut changes: BTreeMap<&str, Vec<&SourceTextEdit>> = BTreeMap::new();
    for edit in &request.edits {
        let text = request
            .lines
            .get(&edit.line_id)
            .ok_or("Transcription edit has no source line")?;
        let source = ScalarText::new(text);
        if edit.start > edit.end
            || edit.end > source.len()
            || source.slice(edit.start..edit.end) != Some(edit.old.as_str())
            || edit.old == edit.text
        {
            return Err("Transcription edit does not match its exact source text".into());
        }
        changes.entry(&edit.line_id).or_default().push(edit);
    }
    let mut lines = request.lines.clone();
    for (id, ordered) in &mut changes {
        ordered.sort_by_key(|edit| (edit.start, edit.end));
        if ordered
            .windows(2)
            .any(|pair| pair[0].end > pair[1].start || pair[0].start == pair[1].start)
        {
            return Err("Transcription edits overlap".into());
        }
        let source = ScalarText::new(&request.lines[*id]);
        let mut text = String::new();
        let mut cursor = 0;
        for edit in ordered.iter() {
            text.push_str(source.slice(cursor..edit.start).unwrap());
            text.push_str(&edit.text);
            cursor = edit.end;
        }
        text.push_str(source.slice(cursor..source.len()).unwrap());
        if text.trim().is_empty() {
            return Err("Transcription cannot erase a retained line".into());
        }
        lines.insert((*id).to_owned(), text);
    }
    let mut anchors = Vec::with_capacity(request.anchors.len());
    for anchor in &request.anchors {
        let span = &anchor.span;
        let before = correction_source(
            span,
            &request.lines,
            matches!(anchor.kind, CorrectionAnchorKind::Normalization),
        )?;
        let ordered = changes
            .get(span.line_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mapped = map_correction_anchor(span, ordered, anchor.kind)?;
        let (start, end) = (mapped.start, mapped.end);
        if matches!(anchor.kind, CorrectionAnchorKind::Evidence)
            && before.slice(span.start..span.end)
                != ScalarText::new(&lines[&span.line_id]).slice(start..end)
        {
            return Err(
                "Transcription changes record evidence; revalidate the extracted fact explicitly"
                    .into(),
            );
        }
        anchors.push(mapped);
    }
    let mut native_anchors = Vec::with_capacity(request.native_anchors.len());
    for span in &request.native_anchors {
        correction_source(span, &request.lines, false)?;
        let ordered = changes
            .get(span.line_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        native_anchors
            .push(map_correction_anchor(span, ordered, CorrectionAnchorKind::Annotation).ok());
    }
    let mut witnesses = BTreeMap::new();
    for (id, spans) in request.witnesses {
        let ordered = changes
            .get(id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut mapped = vec![];
        let source_len = request
            .lines
            .get(&id)
            .ok_or("Witness has no source line")?
            .chars()
            .count();
        let mut previous = 0;
        for span in spans {
            if span.start < previous
                || span.start >= span.end
                || span.end > source_len
                || span.source_start >= span.source_end
                || span.end - span.start != span.source_end - span.source_start
            {
                return Err("Invalid declared witness mapping".into());
            }
            previous = span.end;
            let mut pieces = vec![(span.start, span.end)];
            for edit in ordered {
                pieces = pieces
                    .into_iter()
                    .flat_map(|(a, b)| {
                        if edit.start == edit.end && a < edit.start && edit.start < b {
                            vec![(a, edit.start), (edit.start, b)]
                        } else if edit.end <= a || edit.start >= b {
                            vec![(a, b)]
                        } else {
                            let mut result = vec![];
                            if a < edit.start {
                                result.push((a, edit.start));
                            }
                            if edit.end < b {
                                result.push((edit.end, b));
                            }
                            result
                        }
                    })
                    .collect();
            }
            for (a, b) in pieces {
                let mut start = shifted(a, ordered);
                let end = shifted(b, ordered);
                for edit in ordered {
                    if edit.start == edit.end && edit.start == a {
                        start += edit.text.chars().count();
                    }
                }
                if start < end {
                    mapped.push(WitnessMapping {
                        start,
                        end,
                        source_start: span.source_start + a - span.start,
                        source_end: span.source_start + b - span.start,
                    });
                }
            }
        }
        witnesses.insert(id, mapped);
    }
    Ok(SourceCorrectionResult {
        lines,
        anchors,
        native_anchors,
        witnesses,
    })
}

fn correction_source<'a>(
    span: &SourceSpan,
    lines: &'a BTreeMap<String, String>,
    empty_allowed: bool,
) -> Result<ScalarText<'a>, String> {
    let before = ScalarText::new(
        lines
            .get(&span.line_id)
            .ok_or("Annotation has no source line")?,
    );
    if span.start > span.end
        || (!empty_allowed && span.start == span.end)
        || span.end > before.len()
    {
        return Err("Structural span outside corrected source".into());
    }
    Ok(before)
}

/// Mapping failures describe loss of exact addressability, after input validation.
pub(crate) fn map_correction_anchor(
    span: &SourceSpan,
    edits: &[&SourceTextEdit],
    kind: CorrectionAnchorKind,
) -> Result<SourceSpan, &'static str> {
    let empty_allowed = matches!(kind, CorrectionAnchorKind::Normalization);
    for edit in edits {
        if empty_allowed {
            if !(span.end < edit.start || span.start > edit.end) {
                return Err(
                    "Transcription overlaps manuscript normalization; revise it explicitly",
                );
            }
        } else if (edit.start < span.start && span.start < edit.end)
            || (edit.start < span.end && span.end < edit.end)
            || (edit.start == edit.end && (edit.start == span.start || edit.start == span.end))
        {
            return Err(
                "Transcription crosses an annotation boundary; revise that annotation explicitly",
            );
        }
    }
    let start = shifted(span.start, edits);
    let end = shifted(span.end, edits);
    if start > end || (!empty_allowed && start == end) {
        return Err("Transcription erases an annotation; revise that annotation explicitly");
    }
    Ok(SourceSpan {
        line_id: span.line_id.clone(),
        start,
        end,
    })
}

fn shifted(point: usize, edits: &[&SourceTextEdit]) -> usize {
    let mut removed = 0;
    let mut added = 0;
    for edit in edits {
        if point <= edit.start {
            break;
        }
        removed += edit.end - edit.start;
        added += edit.text.chars().count();
    }
    point - removed + added
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePlacement {
    Before,
    After,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMove {
    pub members: Vec<String>,
    pub anchor: String,
    pub placement: SourcePlacement,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOrderRequest {
    pub order: Vec<String>,
    pub moves: Vec<SourceMove>,
}

/// Move identified source fragments; all other fragments retain their relative order.
pub fn move_source_units(request: SourceOrderRequest) -> Result<Vec<String>, String> {
    use std::collections::HashSet;
    let mut order = request.order;
    let known = order.iter().cloned().collect::<HashSet<_>>();
    if known.len() != order.len() || known.iter().any(String::is_empty) {
        return Err("Source order has empty or duplicate identities".into());
    }
    for movement in request.moves {
        let members = movement.members.iter().collect::<HashSet<_>>();
        if members.is_empty()
            || members.len() != movement.members.len()
            || members.iter().any(|id| !known.contains(*id))
            || !known.contains(&movement.anchor)
            || members.contains(&movement.anchor)
        {
            return Err(
                "Move needs distinct source fragments and an external source anchor".into(),
            );
        }
        order.retain(|id| !members.contains(id));
        let at = order.iter().position(|id| id == &movement.anchor).unwrap()
            + usize::from(matches!(movement.placement, SourcePlacement::After));
        order.splice(at..at, movement.members);
    }
    Ok(order)
}

/// Proven correspondence to an original extraction witness. Gaps are unresolved.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessMapping {
    pub start: usize,
    pub end: usize,
    pub source_start: usize,
    pub source_end: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SourceUnit {
    pub id: String,
    pub text: String,
    pub page: usize,
    pub r#box: Vec<f64>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_range: Option<[usize; 2]>,
    #[serde(default)]
    pub original_text: String,
    #[serde(default)]
    pub witness_mapping: Option<Vec<WitnessMapping>>,
    #[serde(default)]
    pub witness_unresolved: bool,
    #[serde(flatten)]
    pub evidence: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceOperation {
    Split {
        id: String,
        cuts: Vec<usize>,
        #[serde(default)]
        boxes: Vec<Vec<f64>>,
    },
    Insert {
        unit: SourceUnit,
        after: Option<String>,
    },
    Drop {
        id: String,
        duplicate_of: Option<String>,
    },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOperationsRequest {
    pub lines: Vec<SourceUnit>,
    pub order: Vec<String>,
    pub targets: Vec<usize>,
    pub operations: Vec<SourceOperation>,
    #[serde(default)]
    pub anchors: Vec<SourceSpan>,
}
#[derive(Debug, Serialize)]
pub struct SourceOperationsResult {
    pub lines: Vec<SourceUnit>,
    pub order: Vec<String>,
    pub expansion: BTreeMap<String, Vec<String>>,
    pub fragment_ranges: BTreeMap<String, [usize; 2]>,
    pub anchors: Vec<Vec<SourceSpan>>,
}
fn box_valid(value: &[f64]) -> bool {
    value.len() == 4
        && value
            .iter()
            .all(|v| v.is_finite() && (0.0..=1000.0).contains(v))
        && value[0] <= value[2]
        && value[1] <= value[3]
}
fn establish_witness(unit: &mut SourceUnit) -> Result<(), String> {
    let text = ScalarText::new(&unit.text);
    let original = ScalarText::new(&unit.original_text);
    if unit.witness_mapping.is_none() {
        unit.witness_mapping = Some(match unit.source_range {
            Some([a, b]) if original.slice(a..b) == Some(unit.text.as_str()) => {
                vec![WitnessMapping {
                    start: 0,
                    end: text.len(),
                    source_start: a,
                    source_end: b,
                }]
            }
            _ => vec![],
        });
    }
    let spans = unit.witness_mapping.as_ref().unwrap();
    let mut end = 0;
    for span in spans {
        if span.start < end
            || span.start >= span.end
            || span.end > text.len()
            || span.source_start >= span.source_end
            || span.end - span.start != span.source_end - span.source_start
            || text.slice(span.start..span.end)
                != original.slice(span.source_start..span.source_end)
        {
            return Err("Invalid declared original witness correspondence".into());
        }
        end = span.end;
    }
    unit.witness_unresolved = unit.source_id.is_some()
        && !(spans.len() == 1 && spans[0].start == 0 && spans[0].end == text.len());
    unit.source_range = if !unit.witness_unresolved && spans.len() == 1 {
        Some([spans[0].source_start, spans[0].source_end])
    } else {
        None
    };
    Ok(())
}
/// Physical source mutation preserves witnesses without inferring OCR correspondence.
pub fn apply_source_operations(
    request: SourceOperationsRequest,
) -> Result<SourceOperationsResult, String> {
    use std::collections::HashSet;
    let mut units = BTreeMap::new();
    let mut expansion = BTreeMap::new();
    let mut fragment_ranges = BTreeMap::new();
    for mut unit in request.lines {
        if unit.id.is_empty()
            || unit.text.trim().is_empty()
            || unit.page == 0
            || !box_valid(&unit.r#box)
        {
            return Err("Invalid physical source unit".into());
        }
        establish_witness(&mut unit)?;
        expansion.insert(unit.id.clone(), vec![unit.id.clone()]);
        fragment_ranges.insert(unit.id.clone(), [0, unit.text.chars().count()]);
        if units.insert(unit.id.clone(), unit).is_some() {
            return Err("Duplicate source identity".into());
        }
    }
    let unique = request.order.iter().collect::<HashSet<_>>();
    if unique.len() != request.order.len()
        || unique.len() != units.len()
        || request.order.iter().any(|id| !units.contains_key(id))
    {
        return Err("Source order must cover each unit exactly".into());
    }
    let mut order = request.order;
    for operation in request.operations {
        match operation {
            SourceOperation::Split { id, cuts, boxes } => {
                let old = units.get(&id).ok_or("Split has no source unit")?.clone();
                let source = ScalarText::new(&old.text);
                if !request.targets.contains(&old.page)
                    || cuts.is_empty()
                    || cuts[0] == 0
                    || *cuts.last().unwrap() >= source.len()
                    || cuts.windows(2).any(|p| p[0] >= p[1])
                    || (!boxes.is_empty() && boxes.len() != cuts.len() + 1)
                {
                    return Err("Invalid source split".into());
                }
                let boundaries = [vec![0], cuts, vec![source.len()]].concat();
                let mut children = vec![];
                for (index, pair) in boundaries.windows(2).enumerate() {
                    let (a, b) = (pair[0], pair[1]);
                    let child_id = format!("{id}.s{}", index + 1);
                    if units.contains_key(&child_id) {
                        return Err("Split identity already exists".into());
                    }
                    let mut child = old.clone();
                    child.id = child_id.clone();
                    child.text = source.slice(a..b).unwrap().into();
                    child.witness_mapping = Some(
                        old.witness_mapping
                            .as_ref()
                            .unwrap()
                            .iter()
                            .filter_map(|span| {
                                let lo = a.max(span.start);
                                let hi = b.min(span.end);
                                (lo < hi).then(|| WitnessMapping {
                                    start: lo - a,
                                    end: hi - a,
                                    source_start: span.source_start + lo - span.start,
                                    source_end: span.source_start + hi - span.start,
                                })
                            })
                            .collect(),
                    );
                    let mut derived_boxes = vec![];
                    for key in ["spans", "words"] {
                        let mut fragments = vec![];
                        if let Some(values) =
                            old.evidence.get(key).and_then(serde_json::Value::as_array)
                        {
                            for value in values {
                                let Some(start) = value
                                    .get("start")
                                    .and_then(serde_json::Value::as_u64)
                                    .map(|v| v as usize)
                                else {
                                    continue;
                                };
                                let Some(end) = value
                                    .get("end")
                                    .and_then(serde_json::Value::as_u64)
                                    .map(|v| v as usize)
                                else {
                                    continue;
                                };
                                if a <= start && start < end && end <= b {
                                    let mut value = value.clone();
                                    value["start"] = (start - a).into();
                                    value["end"] = (end - a).into();
                                    if let Some(bounds) =
                                        value.get("bbox").and_then(serde_json::Value::as_array)
                                    {
                                        let bounds = bounds
                                            .iter()
                                            .filter_map(serde_json::Value::as_f64)
                                            .collect::<Vec<_>>();
                                        if box_valid(&bounds) {
                                            derived_boxes.push(bounds);
                                        }
                                    }
                                    fragments.push(value);
                                }
                            }
                        }
                        child.evidence.insert(key.into(), fragments.into());
                    }
                    child.r#box = if !boxes.is_empty() {
                        boxes[index].clone()
                    } else if !derived_boxes.is_empty() {
                        vec![
                            derived_boxes
                                .iter()
                                .map(|v| v[0])
                                .fold(f64::INFINITY, f64::min),
                            derived_boxes
                                .iter()
                                .map(|v| v[1])
                                .fold(f64::INFINITY, f64::min),
                            derived_boxes
                                .iter()
                                .map(|v| v[2])
                                .fold(f64::NEG_INFINITY, f64::max),
                            derived_boxes
                                .iter()
                                .map(|v| v[3])
                                .fold(f64::NEG_INFINITY, f64::max),
                        ]
                    } else {
                        old.r#box.clone()
                    };
                    if !box_valid(&child.r#box) || child.text.trim().is_empty() {
                        return Err("Invalid split fragment geometry or text".into());
                    }
                    establish_witness(&mut child)?;
                    units.insert(child_id.clone(), child);
                    fragment_ranges.insert(child_id.clone(), [a, b]);
                    children.push(child_id);
                }
                let at = order.iter().position(|v| v == &id).unwrap();
                order.splice(at..at + 1, children.clone());
                units.remove(&id);
                expansion.insert(id, children);
            }
            SourceOperation::Drop { id, duplicate_of } => {
                let unit = units.get(&id).ok_or("Discard has no source unit")?;
                if !request.targets.contains(&unit.page) {
                    return Err("Discard outside source target".into());
                }
                if let Some(other) = duplicate_of {
                    if other == id || units.get(&other).map(|v| &v.text) != Some(&unit.text) {
                        return Err("Duplicate counterpart differs".into());
                    }
                }
                units.remove(&id);
                order.retain(|v| v != &id);
            }
            SourceOperation::Insert { mut unit, after } => {
                if units.contains_key(&unit.id)
                    || unit.id.is_empty()
                    || unit.text.trim().is_empty()
                    || !request.targets.contains(&unit.page)
                    || !box_valid(&unit.r#box)
                    || unit.source_id.is_some()
                    || !unit.original_text.is_empty()
                {
                    return Err("Invalid inserted source unit".into());
                }
                if units.values().any(|v| {
                    v.page == unit.page
                        && v.r#box[0].max(unit.r#box[0]) < v.r#box[2].min(unit.r#box[2])
                        && v.r#box[1].max(unit.r#box[1]) < v.r#box[3].min(unit.r#box[3])
                        && v.text.contains(&unit.text)
                }) {
                    return Err("Insertion retypes existing source text at its geometry".into());
                }
                let at = if let Some(anchor) = after {
                    let existing = units.get(&anchor).ok_or("Insertion anchor was discarded")?;
                    if existing.page != unit.page {
                        return Err("Insertion anchor belongs to another page".into());
                    }
                    order.iter().position(|v| v == &anchor).unwrap() + 1
                } else {
                    order
                        .iter()
                        .position(|v| units[v].page == unit.page)
                        .unwrap_or(order.len())
                };
                establish_witness(&mut unit)?;
                fragment_ranges.insert(unit.id.clone(), [0, unit.text.chars().count()]);
                order.insert(at, unit.id.clone());
                units.insert(unit.id.clone(), unit);
            }
        }
    }
    let mut anchors = vec![];
    for span in request.anchors {
        let children = expansion
            .get(&span.line_id)
            .ok_or("Anchor has no original source unit")?;
        let original = fragment_ranges[&span.line_id];
        if span.start >= span.end || span.end > original[1] {
            return Err("Anchor exceeds original source unit".into());
        }
        let mut mapped = vec![];
        for id in children {
            if units.contains_key(id) {
                let [a, b] = fragment_ranges[id];
                let lo = a.max(span.start);
                let hi = b.min(span.end);
                if lo < hi {
                    mapped.push(SourceSpan {
                        line_id: id.clone(),
                        start: lo - a,
                        end: hi - a,
                    });
                }
            }
        }
        anchors.push(mapped);
    }
    Ok(SourceOperationsResult {
        lines: order.iter().map(|id| units.remove(id).unwrap()).collect(),
        order,
        expansion,
        fragment_ranges,
        anchors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn apply(value: serde_json::Value) -> Result<SourceCorrectionResult, String> {
        correct_source_text(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn scalar_edits_preserve_distinct_same_line_anchors() {
        let result = apply(json!({
            "lines": {"leaf": "🌿 red blue"},
            "edits": [{"line_id":"leaf","start":2,"end":5,"old":"red","text":"green"}],
            "anchors": [
                {"span":{"line_id":"leaf","start":2,"end":5}},
                {"span":{"line_id":"leaf","start":6,"end":10},"kind":"evidence"},
                {"span":{"line_id":"leaf","start":10,"end":10},"kind":"normalization"}
            ]
        }))
        .unwrap();
        assert_eq!(result.lines["leaf"], "🌿 green blue");
        assert_eq!(
            result
                .anchors
                .iter()
                .map(|s| (s.start, s.end))
                .collect::<Vec<_>>(),
            vec![(2, 7), (8, 12), (12, 12)]
        );
    }

    #[test]
    fn correction_rejects_ambiguous_or_changed_evidence() {
        let base = json!({"lines":{"leaf":"amber fern"},
            "edits":[{"line_id":"leaf","start":0,"end":5,"old":"amber","text":"blue"}],
            "anchors":[{"span":{"line_id":"leaf","start":0,"end":5},"kind":"evidence"}]});
        assert!(apply(base.clone()).unwrap_err().contains("record evidence"));
        let mut crossing = base.clone();
        crossing["anchors"][0] = json!({"span":{"line_id":"leaf","start":2,"end":5}});
        assert!(apply(crossing).unwrap_err().contains("boundary"));
        let mut insertion = base.clone();
        insertion["edits"][0] = json!({"line_id":"leaf","start":0,"end":0,"old":"","text":"pale "});
        assert!(apply(insertion).unwrap_err().contains("boundary"));
        let mut stale = base.clone();
        stale["edits"][0]["old"] = json!("wrong");
        assert!(apply(stale).unwrap_err().contains("exact source"));
        let mut overlap = base.clone();
        overlap["edits"]
            .as_array_mut()
            .unwrap()
            .push(base["edits"][0].clone());
        assert!(apply(overlap).unwrap_err().contains("overlap"));
        let mut normalization = base;
        normalization["anchors"][0] =
            json!({"span":{"line_id":"leaf","start":5,"end":5},"kind":"normalization"});
        assert!(apply(normalization).unwrap_err().contains("normalization"));
    }
}
