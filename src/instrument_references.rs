use crate::{
    instrument::{
        instrument_roman, InstrumentCrossReferenceCounts, InstrumentCrossReferenceEdge,
        InstrumentCrossReferenceGraph, InstrumentCrossReferenceReason,
        InstrumentCrossReferenceStatus, ProvisionReference, ProvisionReferenceShape,
    },
    javascript_whitespace,
    locator::{compact_provision_label, normalize_compact_numbered_section_locator},
    text::{trim_javascript_start, ScalarText},
    DocumentStructure, EngineError, NodeKind, ScalarRange,
};
use legal_grammar_tables::{
    compile_ecmascript_pattern, compile_ecmascript_table_entry, expand_pattern, load_tables,
    CompiledEcmascriptGrammar,
};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

struct ProvisionGrammars {
    reference: CompiledEcmascriptGrammar,
    leading_subdivision: CompiledEcmascriptGrammar,
    external_following: CompiledEcmascriptGrammar,
    instrument_lead: CompiledEcmascriptGrammar,
    list_continuation: CompiledEcmascriptGrammar,
    hyphenated_number: CompiledEcmascriptGrammar,
    thereof: CompiledEcmascriptGrammar,
    list_owner: CompiledEcmascriptGrammar,
    continuation: CompiledEcmascriptGrammar,
}

fn provision_grammars() -> &'static ProvisionGrammars {
    static GRAMMARS: OnceLock<ProvisionGrammars> = OnceLock::new();
    GRAMMARS.get_or_init(|| {
        let tables = load_tables().expect("valid legal grammar corpus");
        let numeric = &tables["provision.reference.numeric"];
        let roman = &tables["provision.reference.roman"];
        let defs = &numeric.defs;
        let continuation = format!(
            r"^\s*(,|and\b|or\b)\s*({}|{})",
            defs.get("numeric_label").expect("numeric label grammar"),
            defs.get("sub_only_label").expect("sub-only label grammar")
        );
        let table = |id| compile_ecmascript_table_entry(id).expect("valid provision grammar");
        ProvisionGrammars {
            reference: compile_ecmascript_pattern(
                "provision.reference",
                &format!(
                    "(?:{})|(?:{})",
                    expand_pattern(&numeric.entry.pattern, &numeric.defs).unwrap(),
                    expand_pattern(&roman.entry.pattern, &roman.defs).unwrap()
                ),
                "i",
            )
            .expect("valid provision grammar"),
            leading_subdivision: table("provision.external.leading-subdivision"),
            external_following: table("provision.external.following"),
            instrument_lead: table("provision.external.instrument-lead"),
            list_continuation: table("provision.external.list-continuation"),
            hyphenated_number: table("provision.external.hyphenated-number"),
            thereof: table("provision.external.thereof"),
            list_owner: table("provision.external.list-owner"),
            continuation: compile_ecmascript_pattern(
                "provision.reference.continuation",
                &continuation,
                "i",
            )
            .expect("valid provision continuation grammar"),
        }
    })
}

#[derive(Default)]
pub(super) struct FindProvisionReferencesOptions<'a> {
    words: Option<&'a [&'a str]>,
    window: Option<usize>,
}

fn replace_first_match<'a>(regex: &CompiledEcmascriptGrammar, value: &'a str) -> &'a str {
    regex
        .find(value)
        .map_or(value, |matched| &value[matched.end()..])
}

fn is_external_reference(following: &str) -> bool {
    let grammars = provision_grammars();
    let trimmed = trim_javascript_start(replace_first_match(
        &grammars.leading_subdivision,
        following,
    ));
    let Some(captures) = grammars.external_following.captures(trimmed) else {
        return false;
    };
    captures
        .get(1)
        .is_some_and(|owner| !owner.as_str().eq_ignore_ascii_case("this"))
}

fn is_external_reference_in_context(before: &str, after: &str) -> bool {
    let grammars = provision_grammars();
    if grammars.instrument_lead.is_match(before)
        || grammars.hyphenated_number.is_match(after)
        || grammars.thereof.is_match(after)
        || is_external_reference(after)
    {
        return true;
    }
    let skipped = replace_first_match(&grammars.list_continuation, after);
    if skipped.len() == after.len() {
        return false;
    }
    grammars
        .list_owner
        .captures(skipped)
        .and_then(|captures| captures.get(1))
        .is_some_and(|owner| !owner.as_str().eq_ignore_ascii_case("this"))
}

fn provision_flanks<'a>(
    text: &'a str,
    start_byte: usize,
    end_byte: usize,
    window: usize,
) -> (&'a str, &'a str) {
    let mut before_byte = start_byte;
    let mut units = 0;
    for (byte, character) in text[..start_byte].char_indices().rev() {
        if units >= window {
            break;
        }
        before_byte = byte;
        units += character.len_utf16();
    }
    let mut after_byte = end_byte;
    units = 0;
    for (byte, character) in text[end_byte..].char_indices() {
        if units >= window {
            break;
        }
        after_byte = end_byte + byte + character.len_utf8();
        units += character.len_utf16();
    }
    (&text[before_byte..start_byte], &text[end_byte..after_byte])
}

#[allow(clippy::too_many_arguments)]
fn push_provision_reference(
    found: &mut Vec<ProvisionReference>,
    text: &str,
    allowed: Option<&HashSet<&str>>,
    window: usize,
    start_byte: usize,
    raw: &str,
    word: &str,
    plural: bool,
    raw_label: &str,
    shape: ProvisionReferenceShape,
    external_override: Option<bool>,
    continuation_of: Option<usize>,
) {
    if found
        .last()
        .is_some_and(|reference| reference.start == start_byte)
    {
        return;
    }
    let mut singular = word.to_lowercase();
    singular.truncate(singular.len() - usize::from(singular.ends_with('s')));
    if allowed.is_some_and(|allowed| !allowed.contains(singular.as_str())) {
        return;
    }
    let label = compact_provision_label(raw_label);
    let end_byte = start_byte + raw.len();
    let external = external_override.unwrap_or_else(|| {
        let (before, after) = provision_flanks(text, start_byte, end_byte, window);
        is_external_reference_in_context(before, after)
    });
    let locator = if shape == ProvisionReferenceShape::Roman {
        String::new()
    } else {
        normalize_compact_numbered_section_locator(&label)
    };
    let alias_key = format!("{singular} {label}").to_lowercase();
    found.push(ProvisionReference {
        start: start_byte,
        end: end_byte,
        raw: raw.to_owned(),
        word: singular,
        plural,
        label,
        shape,
        locator,
        alias_key,
        external,
        continuation_of,
    });
}

pub(super) fn find_provision_references(
    coordinates: &ScalarText<'_>,
    options: FindProvisionReferencesOptions<'_>,
) -> Vec<ProvisionReference> {
    static TRAILING_SUBDIVISIONS: OnceLock<Regex> = OnceLock::new();
    let grammars = provision_grammars();
    let window = options.window.unwrap_or(40);
    let allowed = options
        .words
        .map(|words| words.iter().copied().collect::<HashSet<_>>());
    let text = coordinates.value;
    let mut found = Vec::new();
    let reference_grammar = &grammars.reference;
    let mut capture_locations = reference_grammar.capture_locations();
    let mut search_start = 0;

    while let Some(whole) =
        reference_grammar.captures_read_at(&mut capture_locations, text, search_start)
    {
        search_start = whole.end();
        let capture = |index| {
            capture_locations
                .get(index)
                .map(|(start, end)| &text[start..end])
        };
        if capture(1).is_none() {
            let word = capture(5).expect("roman provision word");
            push_provision_reference(
                &mut found,
                text,
                allowed.as_ref(),
                window,
                whole.start(),
                whole.as_str(),
                word,
                word.ends_with('s') || word.ends_with('S'),
                capture(6).expect("roman provision label"),
                ProvisionReferenceShape::Roman,
                None,
                None,
            );
            continue;
        }
        let raw_label = capture(3).or_else(|| capture(4)).unwrap_or("");
        let start_byte = whole.start();
        let end_byte = whole.end();
        let (before, after) = provision_flanks(text, start_byte, end_byte, window);
        let external = is_external_reference_in_context(before, after);
        let word = capture(1).expect("provision word");
        push_provision_reference(
            &mut found,
            text,
            allowed.as_ref(),
            window,
            start_byte,
            whole.as_str(),
            word,
            capture(2).is_some(),
            raw_label,
            if capture(3).is_some() {
                ProvisionReferenceShape::Numeric
            } else {
                ProvisionReferenceShape::SubOnly
            },
            Some(external),
            None,
        );

        struct Continuation<'a> {
            start_byte: usize,
            raw: &'a str,
            connector: String,
            shape: ProvisionReferenceShape,
        }
        let mut continuations = Vec::new();
        let mut cursor = end_byte;
        for _ in 0..50 {
            let Some(continuation) = grammars.continuation.captures(&text[cursor..]) else {
                break;
            };
            let whole = continuation.get(0).expect("provision continuation match");
            let label = continuation.get(2).expect("provision continuation label");
            let label_at = whole.as_str().rfind(label.as_str()).unwrap();
            let label_start = cursor + label_at;
            continuations.push(Continuation {
                start_byte: label_start,
                raw: &text[label_start..label_start + label.as_str().len()],
                connector: continuation
                    .get(1)
                    .expect("provision continuation connector")
                    .as_str()
                    .to_lowercase(),
                shape: if label.as_str().starts_with('(') {
                    ProvisionReferenceShape::SubOnly
                } else {
                    ProvisionReferenceShape::Numeric
                },
            });
            cursor += whole.end();
        }
        let safe_to_expand = capture(2).is_some()
            || continuations.len() > 1
            || continuations.iter().any(|item| {
                item.connector != "," || item.shape == ProvisionReferenceShape::SubOnly
            });
        if !safe_to_expand {
            continue;
        }
        let numeric_head = TRAILING_SUBDIVISIONS
            .get_or_init(|| Regex::new(r"(?:\([^()]+\))+$").expect("valid subdivision grammar"))
            .replace(&compact_provision_label(raw_label), "")
            .into_owned();
        for continuation in continuations {
            let label = if continuation.shape == ProvisionReferenceShape::SubOnly
                && !numeric_head.is_empty()
            {
                format!(
                    "{}{}",
                    numeric_head,
                    compact_provision_label(continuation.raw)
                )
            } else {
                continuation.raw.to_owned()
            };
            push_provision_reference(
                &mut found,
                text,
                allowed.as_ref(),
                window,
                continuation.start_byte,
                continuation.raw,
                word,
                false,
                &label,
                ProvisionReferenceShape::Numeric,
                Some(external),
                Some(start_byte),
            );
        }
    }
    let mut head = (0, 0);
    found
        .into_iter()
        .map(|mut reference| {
            let start_byte = reference.start;
            let start = coordinates
                .utf16_at_byte(start_byte)
                .expect("reference boundary");
            if let Some(head_byte) = reference.continuation_of {
                assert_eq!(head_byte, head.0, "coordinated-list head precedes member");
                reference.continuation_of = Some(head.1);
            } else {
                head = (start_byte, start);
            }
            reference.start = start;
            reference.end = coordinates
                .utf16_at_byte(reference.end)
                .expect("reference boundary");
            reference
        })
        .collect()
}

fn reference_node_kind(label: &str) -> &'static str {
    if label.starts_with("art") {
        "article"
    } else if label.starts_with("part") {
        "part"
    } else if label.starts_with("div") {
        "division"
    } else if ["sched", "exh", "annex", "app"]
        .iter()
        .any(|prefix| label.starts_with(prefix))
    {
        "schedule"
    } else if label.contains('(') {
        "subsection"
    } else {
        "section"
    }
}

pub(super) fn populate_instrument_node_metadata(
    nodes: &mut [crate::StructureNode],
    text: &ScalarText<'_>,
) {
    for node in nodes
        .iter_mut()
        .filter(|node| node.kind == NodeKind::Section)
    {
        let Some(label) = node.label.as_deref() else {
            continue;
        };
        let kind = reference_node_kind(&label);
        node.locator_kind = Some(kind.to_owned());
        let body = match kind {
            "article" => label.strip_prefix("art"),
            "part" => label.strip_prefix("part"),
            "division" => label.strip_prefix("div"),
            "schedule" if label.starts_with("sched") => label.strip_prefix("sched"),
            "schedule" if label.starts_with("exh") => label.strip_prefix("exh"),
            "schedule" if label.starts_with("annex") => label.strip_prefix("annex"),
            "schedule" => label.strip_prefix("app"),
            _ => label.strip_prefix("sec"),
        }
        .unwrap_or(label);
        let word = match kind {
            "article" => "article",
            "part" => "part",
            "division" => "division",
            "schedule" if label.starts_with("exh") => "exhibit",
            "schedule" if label.starts_with("annex") => "annex",
            "schedule" if label.starts_with("app") => "appendix",
            "schedule" => "schedule",
            _ => "section",
        };
        let raw_head = node
            .content_start
            .and_then(|end| {
                Some((
                    text.byte_at_utf16(node.range.start)?,
                    text.byte_at_utf16(end)?,
                ))
            })
            .and_then(|(start, end)| text.value.get(start..end))
            .map_or("", |value| value.trim_matches(javascript_whitespace));
        let display = if matches!(kind, "section" | "subsection") {
            format!("section {body}").to_lowercase()
        } else if kind == "schedule" {
            let raw_head = raw_head.to_lowercase();
            let mut words = raw_head.split_ascii_whitespace();
            let schedule_word = words.next().unwrap_or(word);
            let raw_value = words.next().unwrap_or(body).trim_end_matches(':');
            let value = raw_value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
                .then_some(raw_value)
                .unwrap_or(body);
            format!("{schedule_word} {value}")
        } else if raw_head.is_empty() {
            format!("{word} {body}")
        } else {
            raw_head
                .trim_end_matches(|character: char| {
                    matches!(character, '\u{2013}' | '\u{2014}' | '-' | '.' | ':')
                })
                .trim_matches(javascript_whitespace)
                .to_lowercase()
        };
        let mut aliases = vec![display];
        if matches!(kind, "article" | "part") {
            if let Ok(value) = body.parse::<usize>() {
                aliases.push(format!("{word} {}", instrument_roman(value)).to_lowercase());
            }
        }
        let label = label.to_lowercase();
        aliases.retain(|alias| alias != &label);
        aliases.dedup();
        node.aliases = (!aliases.is_empty()).then_some(aliases);
        node.marker_range = node.content_start.map(|end| ScalarRange {
            start: node.range.start,
            end,
        });
    }
}

struct ReferenceNode<'a> {
    label: &'a str,
    aliases: &'a [String],
    anchor: Option<&'a str>,
    parent_label: Option<&'a str>,
    kind: &'static str,
    start: usize,
    end: usize,
    depth: usize,
}

fn reference_nodes<'a>(
    graph: &'a DocumentStructure,
    depths: &HashMap<&str, usize>,
) -> Vec<ReferenceNode<'a>> {
    let by_id = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Section && node.label.is_some())
        .map(|node| {
            let label = node.label.as_deref().unwrap();
            let kind = reference_node_kind(&label);
            let parent_label = node
                .parent_id
                .as_deref()
                .and_then(|id| by_id.get(id))
                .and_then(|parent| parent.label.as_deref());
            ReferenceNode {
                label,
                aliases: node.aliases.as_deref().unwrap_or_default(),
                anchor: node.anchor.as_deref(),
                parent_label,
                kind,
                start: node.range.start,
                end: node.range.end,
                depth: depths[node.id.as_str()],
            }
        })
        .collect()
}

fn node_keys(node: &ReferenceNode, label: String) -> Vec<String> {
    let mut keys = vec![label];
    let roman = match node.kind {
        "article" => node
            .label
            .strip_prefix("art")
            .map(|value| ("article", value)),
        "part" => node.label.strip_prefix("part").map(|value| ("part", value)),
        "division" => node
            .label
            .strip_prefix("div")
            .map(|value| ("division", value)),
        _ => None,
    };
    if let Some((word, value)) =
        roman.and_then(|(word, value)| value.parse().ok().map(|n| (word, n)))
    {
        keys.push(format!("{word} {}", instrument_roman(value)).to_lowercase());
    } else if matches!(node.kind, "section" | "subsection") {
        keys.push(node.label.replacen("sec", "section ", 1).to_lowercase());
    }
    keys.sort();
    keys.dedup();
    keys
}

fn label_parent(locator: &str) -> Option<&str> {
    let body = locator.strip_prefix("sec")?;
    let body = body.split('@').next().unwrap_or(body);
    body.ends_with(')').then_some(())?;
    let open = body.rfind('(')?;
    Some(&locator[..3 + open])
}

fn label_depth(locator: &str) -> usize {
    locator.strip_prefix("sec").map_or(1, |body| {
        1 + body.split('@').next().unwrap_or(body).matches('(').count()
    })
}

fn containing_reference_node(
    nodes: &[ReferenceNode],
    ordered: &[usize],
    by_label: &HashMap<&str, usize>,
    position: usize,
) -> Option<usize> {
    let at = ordered.partition_point(|index| nodes[*index].start <= position);
    let mut node = at.checked_sub(1).map(|index| ordered[index]);
    while let Some(index) = node {
        if position < nodes[index].end {
            return Some(index);
        }
        node = nodes[index]
            .parent_label
            .as_deref()
            .and_then(|label| by_label.get(label).copied());
    }
    None
}

fn reference_locator(reference: &ProvisionReference, source: Option<&ReferenceNode>) -> String {
    if !reference.locator.is_empty() {
        return reference.locator.clone();
    }
    if reference.shape == ProvisionReferenceShape::Roman {
        return reference.alias_key.clone();
    }
    if reference.shape != ProvisionReferenceShape::SubOnly {
        return String::new();
    }
    let Some(source) = source else {
        return String::new();
    };
    let Some(body) = source.label.strip_prefix("sec") else {
        return String::new();
    };
    let head = body.split(['(', '@']).next().unwrap_or("");
    (!head.is_empty())
        .then(|| normalize_compact_numbered_section_locator(&format!("{head}{}", reference.label)))
        .unwrap_or_default()
}

fn js_percent(value: f64) -> u64 {
    (value + 0.5).floor() as u64
}

pub(super) fn resolve_instrument_references(
    text: &ScalarText<'_>,
    graph: &DocumentStructure,
    references: Vec<ProvisionReference>,
    depths: &HashMap<&str, usize>,
) -> Result<InstrumentCrossReferenceGraph, EngineError> {
    const MIN_ADDRESSABLE_NODES: usize = 3;
    const MIN_TARGET_REACH: f64 = 0.05;
    const MIN_TARGETS_FOR_REACH: usize = 3;
    const INTEGRITY_GATE: f64 = 0.5;

    let nodes = reference_nodes(graph, depths);
    let mut by_label = HashMap::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        by_label.entry(node.label).or_insert(index);
    }
    let mut ordered = (0..nodes.len()).collect::<Vec<_>>();
    ordered.sort_by_key(|index| (nodes[*index].start, nodes[*index].depth));

    let mut targets =
        HashMap::<String, (Option<usize>, usize)>::with_capacity(nodes.len().saturating_mul(2));
    let mut child_depths = HashMap::<String, HashSet<usize>>::with_capacity(nodes.len());
    let mut top_level_numeric = 0;
    let mut containers = 0;
    static TOP_LEVEL: OnceLock<Regex> = OnceLock::new();
    let top_level = TOP_LEVEL.get_or_init(|| {
        Regex::new(r"(?i)^sec\d{1,8}[a-z]{0,3}(?:[.-]\d{1,8}[a-z]{0,3}){0,3}$")
            .expect("valid top-level provision grammar")
    });
    for (node_index, node) in nodes.iter().enumerate() {
        let label = node.label.to_lowercase();
        if let Some(parent) = label_parent(&label) {
            child_depths
                .entry(parent.to_owned())
                .or_default()
                .insert(label_depth(&label));
        } else if top_level.is_match(&label) {
            top_level_numeric += 1;
        }
        if matches!(node.kind, "article" | "part" | "division") {
            containers += 1;
        }
        let mut add = |key, ambiguity| {
            let target = targets.entry(key).or_insert((Some(node_index), 0));
            target.1 += usize::from(ambiguity);
            if target.0 != Some(node_index) {
                target.0 = None;
            }
        };
        let mut keys = node_keys(node, label);
        keys.extend(node.aliases.iter().map(|key| key.to_lowercase()));
        keys.sort();
        keys.dedup();
        for key in keys {
            add(key, true);
        }
        if let Some(anchor) = node.anchor {
            add(anchor.to_lowercase(), false);
        }
    }
    let numbers_here = |locator: &str| {
        if let Some(parent) = label_parent(locator) {
            child_depths
                .get(parent)
                .is_some_and(|depths| depths.contains(&label_depth(locator)))
        } else if !locator.starts_with("sec") {
            containers >= MIN_ADDRESSABLE_NODES
        } else {
            top_level.is_match(locator) && top_level_numeric >= MIN_ADDRESSABLE_NODES
        }
    };

    let thin = nodes.len() < MIN_ADDRESSABLE_NODES;
    let mut counts = InstrumentCrossReferenceCounts {
        detected: references.len(),
        resolved: 0,
        external: 0,
        unresolved: 0,
        abstained: 0,
        self_loops: 0,
        integrity: 1.0,
    };
    let mut edges = Vec::with_capacity(references.len());
    for reference in references {
        let source_index = containing_reference_node(&nodes, &ordered, &by_label, reference.start);
        let source = source_index.map(|index| &nodes[index]);
        let locator = reference_locator(&reference, source);
        let mut edge = InstrumentCrossReferenceEdge {
            source_start: reference.start,
            source_end: reference.end,
            source_label: source.map(|node| node.label.to_owned()),
            raw: reference.raw,
            raw_label: reference.label,
            normalized_locator: locator,
            target_label: None,
            target_start: None,
            target_end: None,
            status: InstrumentCrossReferenceStatus::External,
            self_loop: false,
            reason: None,
        };
        if reference.external {
            counts.external += 1;
            edge.reason = Some(InstrumentCrossReferenceReason::ExternalInstrument);
        } else if thin {
            counts.abstained += 1;
            edge.status = InstrumentCrossReferenceStatus::Abstained;
            edge.reason = Some(InstrumentCrossReferenceReason::DocumentAbstained);
        } else if edge.normalized_locator.is_empty() {
            counts.abstained += 1;
            edge.status = InstrumentCrossReferenceStatus::Abstained;
            edge.reason = Some(InstrumentCrossReferenceReason::NoContainingSection);
        } else {
            let lowercase = (!edge.normalized_locator.is_ascii()
                || edge
                    .normalized_locator
                    .bytes()
                    .any(|byte| byte.is_ascii_uppercase()))
            .then(|| edge.normalized_locator.to_lowercase());
            let locator_key = lowercase.as_deref().unwrap_or(&edge.normalized_locator);
            let target = targets.get(locator_key);
            if let Some(target) = target.and_then(|value| value.0) {
                counts.resolved += 1;
                edge.status = InstrumentCrossReferenceStatus::Resolved;
                edge.target_label = Some(nodes[target].label.to_owned());
                edge.target_start = Some(nodes[target].start);
                edge.target_end = Some(nodes[target].end);
                edge.self_loop = source.is_some_and(|source| source.label == nodes[target].label);
                counts.self_loops += usize::from(edge.self_loop);
            } else if target.is_some_and(|value| value.1 > 1) {
                counts.abstained += 1;
                edge.status = InstrumentCrossReferenceStatus::Abstained;
                edge.reason = Some(InstrumentCrossReferenceReason::AmbiguousLabel);
            } else if !numbers_here(locator_key) {
                counts.abstained += 1;
                edge.status = InstrumentCrossReferenceStatus::Abstained;
                edge.reason = Some(InstrumentCrossReferenceReason::DepthNotNumbered);
            } else {
                counts.unresolved += 1;
                edge.status = InstrumentCrossReferenceStatus::Unresolved;
                edge.reason = Some(InstrumentCrossReferenceReason::NoSuchProvision);
            }
        }
        edges.push(edge);
    }

    let accepted = counts.resolved + counts.unresolved;
    counts.integrity = if accepted == 0 {
        1.0
    } else {
        counts.resolved as f64 / accepted as f64
    };
    if thin {
        return Ok(InstrumentCrossReferenceGraph {
            edges,
            document_abstained: true,
            note: Some(format!(
                "Cross-reference resolution abstained: the document compiles to {} addressable provision(s), below the {MIN_ADDRESSABLE_NODES} needed for a numbering scheme to check against.",
                nodes.len()
            )),
            counts,
        });
    }
    let (target_count, furthest_target) = edges
        .iter()
        .filter(|edge| edge.status == InstrumentCrossReferenceStatus::Resolved)
        .filter_map(|edge| edge.target_start)
        .fold((0, 0), |(count, furthest), target| {
            (count + 1, furthest.max(target))
        });
    let reach = if text.utf16_len() == 0 {
        1.0
    } else {
        furthest_target as f64 / text.utf16_len() as f64
    };
    let contents_only = target_count >= MIN_TARGETS_FOR_REACH && reach < MIN_TARGET_REACH;
    if contents_only || (accepted > 0 && counts.integrity < INTEGRITY_GATE) {
        for edge in &mut edges {
            if edge.status != InstrumentCrossReferenceStatus::External {
                edge.status = InstrumentCrossReferenceStatus::Abstained;
                edge.reason = Some(InstrumentCrossReferenceReason::DocumentAbstained);
                edge.target_label = None;
                edge.target_start = None;
                edge.target_end = None;
                edge.self_loop = false;
            }
        }
        let note = if contents_only {
            format!(
                "Cross-reference resolution abstained: every one of {} resolved targets lands in the first {}% of the document, so the only numbering the compiler can see is a table of contents, not the provisions.",
                target_count,
                js_percent(reach * 100.0)
            )
        } else {
            format!(
                "Cross-reference resolution abstained: only {} of {accepted} resolvable references ({}%) landed on a compiled provision, below the {}% needed to trust this document's numbering scheme.",
                counts.resolved,
                js_percent(counts.integrity * 100.0),
                js_percent(INTEGRITY_GATE * 100.0)
            )
        };
        counts.abstained += accepted;
        counts.resolved = 0;
        counts.unresolved = 0;
        counts.self_loops = 0;
        return Ok(InstrumentCrossReferenceGraph {
            edges,
            document_abstained: true,
            note: Some(note),
            counts,
        });
    }
    Ok(InstrumentCrossReferenceGraph {
        edges,
        document_abstained: false,
        note: None,
        counts,
    })
}

#[cfg(test)]
mod provision_reference_tests {
    use super::*;
    use crate::locator::normalize_numbered_section_locator;

    const ACACIA_EXTERNAL: &str =
        "â€œGroupâ€ has the meaning ascribed to such term under Section 13(d) of the Exchange Act.";
    const ACACIA_INTERNAL: &str =
        "in fulfilling its obligations under this Agreement, including under Section 5.3.";
    const ACACIA_LIST: &str =
        "the representations and warranties contained in Section 2.3(b), Section 6.3(a) and Section 7.1(f)), (J) any actions taken";
    const ACACIA_ROMAN: &str =
        "satisfaction or waiver of each of the conditions set forth in Article VI (other than those conditions that by their terms";
    const ACACIA_ORIGINAL_AGREEMENT: &str =
        "B. Pursuant to Section 7.4 of the Original Agreement, Parent, Sub and the Company";

    fn find(text: &str) -> Vec<ProvisionReference> {
        find_provision_references(
            &ScalarText::new(text),
            FindProvisionReferencesOptions::default(),
        )
    }

    #[test]
    fn requires_a_nonempty_label() {
        assert!(find("as provided in this section hereof").is_empty());
        assert!(find("each paragraph of this Agreement").is_empty());
    }

    #[test]
    fn reports_utf16_spans_labels_and_locators() {
        let found = find(ACACIA_INTERNAL);
        assert_eq!(found.len(), 1);
        let reference = &found[0];
        assert_eq!(reference.raw, "Section 5.3");
        assert_eq!(
            &ACACIA_INTERNAL[reference.start..reference.end],
            "Section 5.3"
        );
        assert_eq!(reference.word, "section");
        assert_eq!(reference.label, "5.3");
        assert_eq!(reference.locator, "sec5.3");
        assert_eq!(reference.shape, ProvisionReferenceShape::Numeric);
        assert!(!reference.external);

        let astral = find("\u{1f600} Section 5.3");
        assert_eq!((astral[0].start, astral[0].end), (3, 14));
    }

    #[test]
    fn marks_references_to_another_instrument_external() {
        let reference = &find(ACACIA_EXTERNAL)[0];
        assert_eq!(reference.label, "13(d)");
        assert!(reference.external);
        let original = &find(ACACIA_ORIGINAL_AGREEMENT)[0];
        assert_eq!(original.label, "7.4");
        assert!(original.external);
    }

    #[test]
    fn finds_every_member_of_an_explicitly_repeated_list() {
        assert_eq!(
            find(ACACIA_LIST)
                .into_iter()
                .map(|reference| reference.locator)
                .collect::<Vec<_>>(),
            ["sec2.3(b)", "sec6.3(a)", "sec7.1(f)"]
        );
    }

    #[test]
    fn expands_coordinated_lists_without_collapsing_decimals() {
        let text = "sections 150 and 150.1, subsection 160(2) or (3), and sections 170, 171 or 172";
        let found = find(text);
        assert_eq!(
            found
                .iter()
                .map(|reference| reference.locator.as_str())
                .collect::<Vec<_>>(),
            [
                "sec150",
                "sec150.1",
                "sec160(2)",
                "sec160(3)",
                "sec170",
                "sec171",
                "sec172"
            ]
        );
        for reference in found {
            assert_eq!(&text[reference.start..reference.end], reference.raw);
        }
    }

    #[test]
    fn inherits_external_status_across_a_coordinated_list() {
        assert_eq!(
            find("Sections 302 and 906 of the Sarbanes-Oxley Act")
                .into_iter()
                .map(|reference| (reference.locator, reference.external))
                .collect::<Vec<_>>(),
            [("sec302".to_owned(), true), ("sec906".to_owned(), true)]
        );
    }

    #[test]
    fn does_not_expand_an_ambiguous_singleton_comma() {
        assert_eq!(find("Section 5, 2020 was a difficult year").len(), 1);
    }

    #[test]
    fn reads_only_roman_container_numbering() {
        let reference = &find(ACACIA_ROMAN)[0];
        assert_eq!(reference.raw, "Article VI");
        assert_eq!(reference.shape, ProvisionReferenceShape::Roman);
        assert_eq!(reference.locator, "");
        assert_eq!(reference.alias_key, "article vi");
        assert!(find("Section IV of the deed").is_empty());
    }

    #[test]
    fn carries_sub_only_labels_without_normalizing_them() {
        let reference = &find("as described in paragraph (b) above")[0];
        assert_eq!(reference.shape, ProvisionReferenceShape::SubOnly);
        assert_eq!(reference.label, "(b)");
        assert_eq!(reference.locator, "");
        assert_eq!(normalize_numbered_section_locator("8.01(b)"), "sec8.01(b)");
    }

    #[test]
    fn restricts_the_vocabulary_on_request() {
        let text = "Section 5.3 and Schedule 2.1 and paragraph (b)";
        let found = find_provision_references(
            &ScalarText::new(text),
            FindProvisionReferencesOptions {
                words: Some(&["section"]),
                window: None,
            },
        );
        assert_eq!(
            found
                .into_iter()
                .map(|reference| reference.raw)
                .collect::<Vec<_>>(),
            ["Section 5.3"]
        );
    }

    #[test]
    fn accepts_an_empty_lookaround_window() {
        let found = find_provision_references(
            &ScalarText::new("Act Section 5 thereof"),
            FindProvisionReferencesOptions {
                words: None,
                window: Some(0),
            },
        );
        assert!(!found[0].external);
    }

    #[test]
    fn returns_source_order_without_duplicate_starts() {
        let text = format!("{ACACIA_ROMAN} {ACACIA_LIST}");
        let found = find(&text);
        assert!(found.windows(2).all(|pair| pair[0].start < pair[1].start));
    }

    #[test]
    fn classifies_both_flanks_and_external_numbering_literally() {
        for text in [
            "Code Section 59A applies",
            "Treasury Regulation Section 1.482 applies",
            "Exchange Act Section 13(d) applies",
            "Section 1.6011-4(b)(2) applies",
            "Section 262 thereof applies",
        ] {
            assert!(find(text)[0].external, "{text}");
        }
        assert!(!find("Section 8 of this Agreement")[0].external);
        assert!(!find("Sections 7.2 or 7.3 to be satisfied")[0].external);
    }

    #[test]
    fn serializes_the_complete_reference_contract_in_field_order() {
        let serialized = serde_json::to_string(&find("Sections 160(2) or (3)")).unwrap();
        assert_eq!(
            serialized,
            r#"[{"start":0,"end":15,"raw":"Sections 160(2)","word":"section","plural":true,"label":"160(2)","shape":"numeric","locator":"sec160(2)","aliasKey":"section 160(2)","external":false},{"start":19,"end":22,"raw":"(3)","word":"section","plural":false,"label":"160(3)","shape":"numeric","locator":"sec160(3)","aliasKey":"section 160(3)","external":false,"continuationOf":0}]"#
        );
    }
}
