use crate::{
    analyze_instrument, normalize_document_locator, text::equal_fold, utf16_len,
    AuthoritativeTableCell, DocumentKind, DocumentStructure, InstrumentCrossReferenceReason,
    InstrumentCrossReferenceStatus, ScalarText, StructureNode,
};
use legal_grammar_tables::compile_ecmascript_pattern;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

fn compile(pattern: &str, flags: &str) -> Regex {
    compile_ecmascript_pattern("amendment", pattern, flags).expect("valid amendment grammar")
}
macro_rules! cached {
    ($slot:ident, $pattern:literal, $flags:literal) => {{
        static $slot: OnceLock<Regex> = OnceLock::new();
        $slot.get_or_init(|| compile($pattern, $flags))
    }};
}


struct Analyzed {
    structure: DocumentStructure,
}

fn analyze(text: &str, reconstruct_lineation: bool) -> Result<Analyzed, crate::EngineError> {
    let structure = analyze_instrument(
        text,
        String::new(),
        &[] as &[AuthoritativeTableCell],
        reconstruct_lineation,
    )?;
    Ok(Analyzed { structure })
}


struct Splice {
    start: usize,
    end: usize,
    replacement: String,
    receipt: Value,
}


fn occurrence_base(label: &str) -> &str {
    let Some(at) = label.rfind('@') else {
        return label;
    };
    if !label[at + 1..].is_empty() && label[at + 1..].bytes().all(|byte| byte.is_ascii_digit()) {
        &label[..at]
    } else {
        label
    }
}

fn numbering_parent(label: &str) -> String {
    let Some(body) = occurrence_base(label).strip_prefix("sec") else {
        return String::new();
    };
    if body.ends_with(')') {
        return format!("sec{}", &body[..body.rfind('(').unwrap_or(0)]);
    }
    body.rfind('.')
        .map_or_else(String::new, |at| format!("sec{}", &body[..at]))
}

fn at_or_below(label: &str, root: &str) -> bool {
    let label = occurrence_base(label);
    label == root
        || label.starts_with(&format!("{root}("))
        || label.starts_with(&format!("{root}."))
}

fn numbering_family(label: &str, family: &str) -> bool {
    let label = occurrence_base(label);
    label.starts_with("sec")
        && if family.is_empty() {
            true
        } else {
            label != family && at_or_below(label, family)
        }
}

fn closes_one_step(from: &str, to: &str) -> bool {
    static PATTERNS: OnceLock<[Regex; 3]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            Regex::new(r"^(.*?)(\d+)$").unwrap(),
            Regex::new(r"^(.*)\((\d+)\)$").unwrap(),
            Regex::new(r"^(.*)\(([a-z])\)$").unwrap(),
        ]
    });
    for pattern in patterns {
        let (Some(next), Some(previous)) = (
            pattern.captures(occurrence_base(from)),
            pattern.captures(occurrence_base(to)),
        ) else {
            continue;
        };
        if next[1] != previous[1] {
            continue;
        }
        let ordinal = |value: &str| {
            value
                .parse::<u32>()
                .unwrap_or_else(|_| value.chars().next().unwrap() as u32)
        };
        return ordinal(&next[2]) == ordinal(&previous[2]) + 1;
    }
    false
}

#[derive(Clone, Serialize)]
struct Move {
    from: String,
    to: String,
    #[serde(skip)]
    node: usize,
}

fn mapped_locator(locator: &str, mapping: &[Move]) -> Option<String> {
    mapping
        .iter()
        .filter(|item| at_or_below(locator, &item.from))
        .max_by_key(|item| item.from.len())
        .map(|item| format!("{}{}", item.to, &locator[item.from.len()..]))
}

fn leading_label_span(
    coordinates: &ScalarText<'_>,
    node: &StructureNode,
) -> Option<(usize, usize, String)> {
    let range = node.marker_range?;
    let marker = coordinates.slice_utf16(range.start..range.end)?;
    let found = cached!(
        LEADING_LABEL,
        r"(\([^\s()]{1,12}\)|\d+[A-Za-z]?(?:[.-]\d+[A-Za-z]?)*\.?)\s*$",
        ""
    )
    .captures(marker)?
    .get(1)?;
    let start = range.start + utf16_len(&marker[..found.start()]);
    Some((
        start,
        start + utf16_len(found.as_str()),
        found.as_str().to_owned(),
    ))
}

fn heading_token(label: &str, old: &str) -> String {
    let body = label.strip_prefix("sec").unwrap_or(label);
    let mut token = if old.starts_with('(') {
        body.rfind('(').map_or(body, |at| &body[at..]).to_owned()
    } else {
        body.find('(').map_or(body, |at| &body[..at]).to_owned()
    };
    if old.ends_with('.') && !token.ends_with('.') {
        token.push('.')
    }
    token
}

fn reference_text(raw: &str, raw_label: &str, locator: &str) -> String {
    let full = locator.strip_prefix("sec").unwrap_or(locator);
    let label = if raw_label.starts_with('(') {
        let depth = raw_label.matches('(').count();
        let subs = cached!(SUBPROVISIONS, r"\([^()]+\)", "")
            .find_iter(full)
            .map(|item| item.as_str())
            .collect::<Vec<_>>();
        subs[subs.len().saturating_sub(depth)..].join("")
    } else {
        full.to_owned()
    };
    if label == raw_label {
        return raw.to_owned();
    }
    match cached!(REFERENCE_START, r"[\d(]", "").find(raw) {
        Some(at) => format!("{}{}", &raw[..at.start()], label),
        None => raw.to_owned(),
    }
}

fn delete_failure(code: &str, detail: String, range: Option<(usize, usize)>) -> Value {
    let mut value = serde_json::Map::from_iter([
        ("code".to_owned(), Value::String(code.to_owned())),
        ("detail".to_owned(), Value::String(detail)),
    ]);
    if let Some((start, end)) = range {
        value.insert("start".to_owned(), json!(start));
        value.insert("end".to_owned(), json!(end));
    }
    Value::Object(value)
}

fn delete_failed(source: &str, mapping: &[Move], failures: Vec<Value>) -> Value {
    json!({
        "text": source,
        "mapping": mapping,
        "applied": [],
        "failures": failures,
        "verification": { "headingsRenumbered": 0, "referencesUpdated": 0 }
    })
}

fn delete_error(
    source: &str,
    mapping: &[Move],
    code: &str,
    detail: String,
    range: Option<(usize, usize)>,
) -> Value {
    delete_failed(source, mapping, vec![delete_failure(code, detail, range)])
}

fn serialized_name(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

pub fn delete_provision_and_renumber_siblings(
    source: &str,
    target: &str,
    reconstruct_lineation: bool,
) -> Result<Value, crate::EngineError> {
    let before = analyze(source, reconstruct_lineation)?;
    let coordinates = ScalarText::new(source);
    let requested = if target.to_lowercase().starts_with("sec") {
        target.to_lowercase()
    } else {
        normalize_document_locator(DocumentKind::Section, target).to_lowercase()
    };
    let matches = before
        .structure
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.label
                .as_deref()
                .is_some_and(|label| equal_fold(occurrence_base(label), &requested))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if requested.is_empty() || matches.is_empty() {
        return Ok(delete_error(
            source,
            &[],
            "target_not_found",
            target.to_owned(),
            None,
        ));
    }
    if matches.len() > 1 {
        return Ok(delete_error(
            source,
            &[],
            "target_ambiguous",
            format!("{target} resolves to {} provisions", matches.len()),
            None,
        ));
    }
    let selected = &before.structure.nodes[matches[0]];
    let selected_label = selected.label.as_deref().unwrap();
    if !matches!(
        selected.locator_kind.as_deref(),
        Some("section" | "subsection")
    ) {
        return Ok(delete_error(
            source,
            &[],
            "unsupported_target",
            format!(
                "{selected_label} is a {} locator",
                selected.locator_kind.as_deref().unwrap_or("non-provision")
            ),
            None,
        ));
    }
    let family = numbering_parent(selected_label);
    let mut siblings = before
        .structure
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.locator_kind == selected.locator_kind
                && node.parent_id == selected.parent_id
                && node
                    .label
                    .as_deref()
                    .is_some_and(|label| numbering_parent(label) == family)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    siblings.sort_by_key(|index| before.structure.nodes[*index].range.start);
    let bases = siblings
        .iter()
        .map(|index| occurrence_base(before.structure.nodes[*index].label.as_deref().unwrap()))
        .collect::<Vec<_>>();
    if bases.iter().copied().collect::<HashSet<_>>().len() != bases.len() {
        return Ok(delete_error(
            source,
            &[],
            "sibling_ambiguous",
            format!("The sibling sequence containing {selected_label} repeats a label"),
            None,
        ));
    }
    let selected_at = siblings
        .iter()
        .position(|index| *index == matches[0])
        .unwrap();
    let following = &siblings[selected_at + 1..];
    let mapping = following
        .iter()
        .enumerate()
        .map(|(index, node)| Move {
            from: before.structure.nodes[*node].label.clone().unwrap(),
            to: if index == 0 {
                selected_label.to_owned()
            } else {
                before.structure.nodes[following[index - 1]]
                    .label
                    .clone()
                    .unwrap()
            },
            node: *node,
        })
        .collect::<Vec<_>>();
    if let Some(step) = mapping
        .iter()
        .find(|step| !closes_one_step(&step.from, &step.to))
    {
        return Ok(delete_error(
            source,
            &mapping,
            "sibling_sequence_unsupported",
            format!(
                "Cannot prove that {} immediately follows {}; existing gaps or unsupported numbering must not be compressed",
                step.from, step.to
            ),
            None,
        ));
    }
    let mut failures = Vec::new();
    let mut splices = vec![Splice {
        start: selected.range.start,
        end: selected.range.end,
        replacement: String::new(),
        receipt: json!({
            "kind": "delete_provision", "start": selected.range.start, "end": selected.range.end,
            "removed": coordinates.slice_utf16(selected.range.start..selected.range.end).unwrap_or_default(), "inserted": "",
            "from": selected_label, "to": null
        }),
    }];
    let mut heading_spans = Vec::new();
    for step in &mapping {
        let node = &before.structure.nodes[step.node];
        let Some((start, end, old)) = leading_label_span(&coordinates, node) else {
            failures.push(delete_failure(
                "heading_not_found",
                format!("No leading label token at {}", step.from),
                Some((node.range.start, node.range.end.min(node.range.start + 100))),
            ));
            continue;
        };
        let inserted = heading_token(&step.to, &old);
        heading_spans.push((start, end));
        splices.push(Splice {
            start,
            end,
            replacement: inserted.clone(),
            receipt: json!({
                "kind": "renumber_heading", "start": start, "end": end,
                "removed": coordinates.slice_utf16(start..end).unwrap_or_default(), "inserted": inserted,
                "from": step.from, "to": step.to
            }),
        });
    }
    for edge in before
        .structure
        .cross_references
        .as_ref()
        .into_iter()
        .flat_map(|graph| &graph.edges)
    {
        if edge.source_start >= selected.range.start && edge.source_end <= selected.range.end {
            continue;
        }
        let locator = occurrence_base(&edge.normalized_locator);
        if locator.is_empty() || !numbering_family(locator, &family) {
            continue;
        }
        if edge.status != InstrumentCrossReferenceStatus::Resolved {
            let code = if edge.status == InstrumentCrossReferenceStatus::External {
                "external_reference"
            } else if edge.reason == Some(InstrumentCrossReferenceReason::AmbiguousLabel) {
                "ambiguous_reference"
            } else {
                "unresolved_reference"
            };
            failures.push(delete_failure(
                code,
                format!(
                    "{}: {}",
                    edge.raw,
                    edge.reason
                        .map_or_else(|| serialized_name(edge.status), serialized_name)
                ),
                Some((edge.source_start, edge.source_end)),
            ));
            continue;
        }
        if edge
            .target_label
            .as_deref()
            .is_some_and(|label| at_or_below(label, selected_label))
        {
            failures.push(delete_failure(
                "reference_to_deleted_target",
                format!("{} points to {selected_label}", edge.raw),
                Some((edge.source_start, edge.source_end)),
            ));
            continue;
        }
        let Some(moved) = mapped_locator(locator, &mapping) else {
            continue;
        };
        if heading_spans
            .iter()
            .any(|(start, end)| edge.source_start < *end && edge.source_end > *start)
        {
            continue;
        }
        let inserted = reference_text(&edge.raw, &edge.raw_label, &moved);
        if inserted == edge.raw {
            continue;
        }
        splices.push(Splice {
            start: edge.source_start,
            end: edge.source_end,
            replacement: inserted.clone(),
            receipt: json!({
                "kind": "update_cross_reference", "start": edge.source_start, "end": edge.source_end,
                "removed": coordinates.slice_utf16(edge.source_start..edge.source_end).unwrap_or_default(), "inserted": inserted,
                "from": locator, "to": moved
            }),
        });
    }
    if !failures.is_empty() {
        return Ok(delete_failed(source, &mapping, failures));
    }
    splices.sort_by_key(|splice| (splice.start, splice.end));
    for pair in splices.windows(2) {
        if pair[1].start < pair[0].end {
            return Ok(delete_failed(
                source,
                &mapping,
                vec![delete_failure(
                    "overlapping_ops",
                    format!(
                        "{}-{} overlaps {}-{}",
                        pair[1].start, pair[1].end, pair[0].start, pair[0].end
                    ),
                    None,
                )],
            ));
        }
    }
    let mut text = source.to_owned();
    for splice in splices.iter().rev() {
        let start = coordinates.byte_at_utf16(splice.start).unwrap();
        let end = coordinates.byte_at_utf16(splice.end).unwrap();
        text.replace_range(start..end, &splice.replacement);
    }
    let after = analyze(&text, reconstruct_lineation)?;
    let mut counts = HashMap::<String, usize>::new();
    for label in after
        .structure
        .nodes
        .iter()
        .filter_map(|node| node.label.as_deref())
    {
        *counts.entry(occurrence_base(label).to_owned()).or_default() += 1;
    }
    let vacated = mapping.last().map_or(selected_label, |step| &step.from);
    if mapping.iter().any(|step| counts.get(&step.to) != Some(&1))
        || counts.get(vacated).copied().unwrap_or(0) != 0
    {
        return Ok(delete_failed(
            source,
            &mapping,
            vec![delete_failure(
                "verification_failed",
                format!("Renumbered structure did not compile uniquely; vacated {vacated}"),
                None,
            )],
        ));
    }
    let headings = splices
        .iter()
        .filter(|splice| splice.receipt["kind"] == "renumber_heading")
        .count();
    let references = splices
        .iter()
        .filter(|splice| splice.receipt["kind"] == "update_cross_reference")
        .count();
    Ok(json!({
        "text": text,
        "mapping": mapping,
        "applied": splices.into_iter().map(|splice| splice.receipt).collect::<Vec<_>>(),
        "failures": [],
        "verification": { "headingsRenumbered": headings, "referencesUpdated": references }
    }))
}
