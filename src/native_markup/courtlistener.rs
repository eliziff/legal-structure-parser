//! CourtListener's own renditions (html_with_citations, CAP's xml_harvard, Columbia, Lawbox, Lexis,
//! Westlaw, OSCN, public.resource.org): what their markup and printed text supply beyond the generic reading.

use super::*;
use crate::{Derivation, Note, NoteKindV2};

/// Exact markup insertions over a retained source rendition. Offsets are UTF-8
/// bytes in the source after HTML escaping, when it is plain text.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CourtListenerMarkupRecipe {
    pub source_is_plain_text: bool,
    pub insertions: Vec<(usize, String)>,
}

pub(super) fn courtlistener_source(source: &str, plain_text: bool) -> Cow<'_, str> {
    if !plain_text {
        return Cow::Borrowed(source);
    }
    let mut escaped = String::with_capacity(source.len());
    for character in source.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            _ => escaped.push(character),
        }
    }
    Cow::Owned(escaped)
}

/// Find CourtListener's citation/pre wrappers without reserializing source
/// markup. Admit a recipe only when it rebuilds the independently supplied
/// annotated rendition exactly; unfamiliar transformations yield None.
pub fn courtlistener_markup_recipe(
    source: &str,
    annotated: &str,
    source_is_plain_text: bool,
) -> Option<CourtListenerMarkupRecipe> {
    let mut stack = Vec::<(String, bool, bool)>::new();
    let mut spans = Vec::new();
    let mut attributes = Attributes::new();
    let mut at = 0;
    while let Some(relative) = annotated[at..].find('<') {
        at += relative;
        if annotated[at..].starts_with("<!--") {
            at += 4 + annotated[at + 4..].find("-->")? + 3;
            continue;
        }
        let end = at + annotated[at..].find('>')? + 1;
        let raw = &annotated[at..end];
        if let Some((tag, _)) = parse_tag(raw, true) {
            if let Some(index) = stack.iter().rposition(|entry| entry.0 == tag) {
                if stack[index].1 {
                    spans.push(at..end);
                }
                stack.truncate(index);
            }
        } else if let Some((tag, attrs)) = parse_tag(raw, false) {
            if !void_tag(&tag) && !raw[..raw.len() - 1].trim_end().ends_with('/') {
                parse_attributes(attrs, &mut attributes);
                let classes = attribute(&attributes, "class");
                let citation = tag == "span" && contains_ascii_word(classes, "citation", false);
                let remove = citation
                    || (tag == "a" && stack.last().is_some_and(|entry| entry.2))
                    || (source_is_plain_text
                        && tag == "pre"
                        && classes
                            .split_ascii_whitespace()
                            .eq(std::iter::once("inline")));
                if remove {
                    spans.push(at..end);
                }
                stack.push((tag, remove, citation));
            }
        }
        at = end;
    }
    let mut base = String::with_capacity(annotated.len());
    let mut insertions = Vec::with_capacity(spans.len());
    let mut cursor = 0;
    for span in spans {
        base.push_str(annotated.get(cursor..span.start)?);
        insertions.push((base.len(), annotated.get(span.clone())?.to_owned()));
        cursor = span.end;
    }
    base.push_str(&annotated[cursor..]);
    if base != courtlistener_source(source, source_is_plain_text) {
        return None;
    }
    let recipe = CourtListenerMarkupRecipe {
        source_is_plain_text,
        insertions,
    };
    (restore_courtlistener_markup(source, &recipe).ok()?.as_str() == annotated).then_some(recipe)
}

/// Replay retained annotation bytes without HTML parsing or citation matching.
pub fn restore_courtlistener_markup(
    source: &str,
    recipe: &CourtListenerMarkupRecipe,
) -> Result<String, EngineError> {
    let base = courtlistener_source(source, recipe.source_is_plain_text);
    let mut result = String::with_capacity(base.len());
    let mut cursor = 0;
    for (at, insertion) in &recipe.insertions {
        result
            .push_str(base.get(cursor..*at).ok_or_else(|| {
                EngineError::source("invalid CourtListener insertion byte range")
            })?);
        result.push_str(insertion);
        cursor = *at;
    }
    result.push_str(
        base.get(cursor..)
            .ok_or_else(|| EngineError::source("invalid CourtListener source byte range"))?,
    );
    Ok(result)
}

pub(super) fn courtlistener_sibling_note_markers(
    elements: &[MarkupElement],
    text: &str,
) -> HashMap<usize, (String, ScalarRange)> {
    let mut coordinates = None;
    let mut numbers = HashMap::<usize, Option<(String, ScalarRange)>>::new();
    let mut bodies = HashMap::<usize, Option<usize>>::new();
    for element in elements.iter().filter(|element| element.tag == "div") {
        let Some(parent) = element.parent else {
            continue;
        };
        let classes = element.attributes.get("class").map_or("", String::as_str);
        if classes
            .split_ascii_whitespace()
            .any(|class| class == "co_footnoteBody")
        {
            bodies
                .entry(parent)
                .and_modify(|body| *body = None)
                .or_insert(Some(element.markup_start));
        }
        if !classes
            .split_ascii_whitespace()
            .any(|class| class == "co_footnoteNumber")
        {
            continue;
        }
        let coordinates = coordinates.get_or_insert_with(|| ScalarText::new(text));
        let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end) else {
            continue;
        };
        let label = trim_js(raw);
        if label.is_empty() || label.len() > 5 || !label.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
        let start = element.range.start + utf16_len(&raw[..leading]);
        let marker = (
            format!("fn{}", label.parse::<u32>().unwrap()),
            ScalarRange {
                start,
                end: start + utf16_len(label),
            },
        );
        numbers
            .entry(parent)
            .and_modify(|number| *number = None)
            .or_insert(Some(marker));
    }
    bodies
        .into_iter()
        .filter_map(|(parent, body)| Some((body?, numbers.remove(&parent)??)))
        .collect()
}

pub(super) fn courtlistener_footnote_number(raw: &str) -> Option<PendingBlock> {
    let marker = trim_js(raw);
    let number = marker.strip_prefix("[fn")?.strip_suffix(']')?;
    if number.is_empty() || number.len() > 5 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut identity = footnote_identity(number, "", None)?;
    identity.aliases = vec![marker.to_owned(), format!("footnote {marker}")];
    Some(identity)
}

pub(super) fn courtlistener_numbered_note_markers(
    elements: &[MarkupElement],
    matched_role_starts: &HashSet<usize>,
    text: &str,
) -> HashMap<usize, (String, Vec<String>, ScalarRange)> {
    if !elements
        .iter()
        .any(|element| element.tag == "footnote_number")
    {
        return HashMap::new();
    }
    let coordinates = ScalarText::new(text);
    let mut markers = HashMap::<usize, Option<(String, Vec<String>, ScalarRange)>>::new();
    for element in elements
        .iter()
        .filter(|element| element.tag == "footnote_number")
    {
        let Some(body) = element
            .parent
            .map(|parent| &elements[parent])
            .filter(|body| {
                body.tag == "footnote_body" && matched_role_starts.contains(&body.markup_start)
            })
        else {
            continue;
        };
        let marker = (|| {
            if !matched_role_starts.contains(&element.markup_start) {
                return None;
            }
            let raw = coordinates.slice_utf16(element.range.start..element.range.end)?;
            let identity = courtlistener_footnote_number(raw)?;
            let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
            let start = element.range.start + utf16_len(&raw[..leading]);
            let range = ScalarRange {
                start,
                end: start + utf16_len(trim_js(raw)),
            };
            if body.range.end < range.end
                || !trim_js(coordinates.slice_utf16(body.range.start..start)?).is_empty()
            {
                return None;
            }
            Some((identity.label, identity.aliases, range))
        })();
        // A merged body with multiple declared numbers supplies no unique marker.
        markers
            .entry(body.markup_start)
            .and_modify(|marker| *marker = None)
            .or_insert(marker);
    }
    markers
        .into_iter()
        .filter_map(|(start, marker)| Some((start, marker?)))
        .collect()
}

pub(super) fn supplement_courtlistener_declared_note_references(
    structure: &mut DocumentStructure,
    matched_role_starts: &HashSet<usize>,
) {
    let updates = {
        let elements = &structure.markup_elements;
        if structure.notes.is_empty() {
            return;
        }
        let has_class = |element: &MarkupElement, class: &str| {
            element
                .attributes
                .get("class")
                .is_some_and(|classes| classes.split_ascii_whitespace().any(|value| value == class))
        };
        let ldml_reference = |element: &MarkupElement| {
            element.tag == "sup"
                && has_class(element, "ldml-superscript")
                && matched_role_starts.contains(&element.markup_start)
        };
        let has_ldml_references = elements.iter().any(ldml_reference);
        if !has_ldml_references
            && !elements.iter().any(|element| {
                element.tag == "footnotemark"
                    || element.tag == "footnote_reference"
                    || element.tag.eq_ignore_ascii_case("footnotereference")
            })
        {
            return;
        }
        let opinion_owner = |index: usize| {
            let mut parent = elements[index].parent;
            while let Some(index) = parent {
                if elements[index].tag == "opinion" {
                    return Some(index);
                }
                parent = elements[index].parent;
            }
            None
        };
        // Each opinion's notes by label, in source order: a label can repeat ("*", or a numbering restarted).
        let mut owners = HashMap::<(usize, &str), Vec<usize>>::new();
        for (index, element) in elements.iter().enumerate() {
            if element.tag != "footnote" {
                continue;
            }
            let Some(opinion) = opinion_owner(index) else {
                continue;
            };
            let Some(label) = element.attributes.get("label").map(|label| trim_js(label)) else {
                continue;
            };
            if !label.is_empty() {
                owners.entry((opinion, label)).or_default().push(index);
            }
        }
        let nodes = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Footnote)
            .filter_map(|node| Some((node.markup_start?, node.id.as_str())))
            .collect::<HashMap<_, _>>();
        let notes = structure
            .notes
            .iter()
            .enumerate()
            .filter_map(|(index, note)| note.node_id.as_deref().map(|id| (id, index)))
            .collect::<HashMap<_, _>>();
        let mut identities = HashMap::<&str, Option<usize>>::new();
        let mut direct_targets = HashMap::<&str, Option<usize>>::new();
        if elements.iter().any(|element| {
            element.tag == "footnote_reference"
                || element.tag.eq_ignore_ascii_case("footnotereference")
        }) {
            for (index, element) in elements.iter().enumerate() {
                if let Some(id) = element.attributes.get("id").filter(|id| !id.is_empty()) {
                    identities
                        .entry(id.as_str())
                        .and_modify(|identity| *identity = None)
                        .or_insert(Some(index));
                }
            }
            for node in structure
                .nodes
                .iter()
                .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote)
            {
                let Some(start) = node.markup_start else {
                    continue;
                };
                let Ok(index) =
                    elements.binary_search_by_key(&start, |element| element.markup_start)
                else {
                    continue;
                };
                let element = &elements[index];
                let Some(id) = element.attributes.get("id") else {
                    continue;
                };
                if identities.get(id.as_str()).copied().flatten() != Some(index)
                    || !node
                        .markup_tag
                        .as_deref()
                        .is_some_and(|tag| tag.eq_ignore_ascii_case(&element.tag))
                {
                    continue;
                }
                let Some(&note) = notes.get(node.id.as_str()) else {
                    continue;
                };
                direct_targets
                    .entry(id.as_str())
                    .and_modify(|target| *target = None)
                    .or_insert(Some(note));
            }
        }
        let coordinates = ScalarText::new(structure.query_text());
        let mut numbered_targets = HashMap::<&str, Option<usize>>::new();
        if elements
            .iter()
            .any(|element| element.tag == "footnote_reference")
        {
            for element in elements
                .iter()
                .filter(|element| element.tag == "footnote_number")
            {
                let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end)
                else {
                    continue;
                };
                if courtlistener_footnote_number(raw).is_none() {
                    continue;
                }
                let label = trim_js(raw);
                let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
                let start = element.range.start + utf16_len(&raw[..leading]);
                let range = ScalarRange {
                    start,
                    end: start + utf16_len(label),
                };
                let target = element
                    .parent
                    .filter(|parent| {
                        elements[*parent].tag == "footnote_body"
                            && matched_role_starts.contains(&elements[*parent].markup_start)
                            && matched_role_starts.contains(&element.markup_start)
                    })
                    .and_then(|parent| nodes.get(&elements[parent].markup_start))
                    .and_then(|node| notes.get(node))
                    .copied()
                    .filter(|note| structure.notes[*note].label_range == range);
                // Repeated declarations, including an unqualified second body,
                // cannot establish a unique whole-field reference relationship.
                numbered_targets
                    .entry(label)
                    .and_modify(|target| *target = None)
                    .or_insert(target);
            }
        }
        let is_ldml = |element: &MarkupElement, class: &str| {
            element.tag == "div" && has_class(element, class)
        };
        let mut decision_owners = Vec::<Option<usize>>::new();
        let mut writing_owners = Vec::<Option<usize>>::new();
        let mut writings = HashMap::<usize, Option<usize>>::new();
        let mut footers = HashMap::<usize, Option<usize>>::new();
        if has_ldml_references {
            for (index, element) in elements.iter().enumerate() {
                let decision = element.parent.and_then(|parent| {
                    if is_ldml(&elements[parent], "ldml-decision") {
                        Some(parent)
                    } else {
                        decision_owners[parent]
                    }
                });
                let writing = element.parent.and_then(|parent| {
                    if is_ldml(&elements[parent], "ldml-opinion") {
                        Some(parent)
                    } else {
                        writing_owners[parent]
                    }
                });
                decision_owners.push(decision);
                writing_owners.push(writing);
                if let Some(decision) = decision {
                    let owners = if is_ldml(element, "ldml-opinion") {
                        Some(&mut writings)
                    } else if is_ldml(element, "ldml-notes") {
                        Some(&mut footers)
                    } else {
                        None
                    };
                    if let Some(owners) = owners {
                        // Even an unmatched or indirect second declaration is ambiguous.
                        owners
                            .entry(decision)
                            .and_modify(|owner| *owner = None)
                            .or_insert(Some(index));
                    }
                }
            }
        }
        let ldml_scope = |decision: usize| {
            let writing = writings.get(&decision).copied().flatten()?;
            let footer = footers.get(&decision).copied().flatten()?;
            (elements[writing].parent == Some(decision)
                && elements[footer].parent == Some(decision)
                && elements[writing].markup_end <= elements[footer].markup_start
                && [decision, writing, footer]
                    .iter()
                    .all(|index| matched_role_starts.contains(&elements[*index].markup_start)))
            .then_some((writing, footer))
        };
        let mut ldml_targets = HashMap::<(usize, &str), Option<usize>>::new();
        if has_ldml_references {
            let native_owners = structure
                .nodes
                .iter()
                .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote)
                .filter_map(|node| {
                    Some((
                        node.markup_start?,
                        (Kind::Footnote, node.range.start, node.range.end),
                    ))
                })
                .collect::<HashMap<_, _>>();
            for (index, element) in elements.iter().enumerate() {
                if !courtlistener_ldml_note(element) {
                    continue;
                }
                let Some(decision) = decision_owners[index] else {
                    continue;
                };
                let Some((_, marker)) = courtlistener_declared_note_marker(
                    elements,
                    index,
                    &coordinates,
                    &native_owners,
                ) else {
                    continue;
                };
                let Some(label) = coordinates
                    .slice_utf16(marker.start..marker.end)
                    .filter(|label| courtlistener_ldml_bracketed_marker(label))
                else {
                    continue;
                };
                let target = ldml_scope(decision).and_then(|(_, footer)| {
                    let note = nodes
                        .get(&element.markup_start)
                        .and_then(|node| notes.get(node))
                        .copied()?;
                    let body = &structure.notes[note];
                    (element.parent == Some(footer)
                        && writing_owners[index].is_none()
                        && matched_role_starts.contains(&element.markup_start)
                        && native_owners.contains_key(&element.markup_start)
                        && body.label_range == marker
                        && body.body_range.start == marker.end
                        && body.body_range.end == element.range.end
                        && body.references.is_empty()
                        && body.primary_reference.is_none()
                        && elements[index + 1..]
                            .iter()
                            .take_while(|child| child.markup_start < element.markup_end)
                            .any(|child| ldml_reference(child) && child.range == marker))
                    .then_some(note)
                });
                // Count literal labels even when a body cannot supply a qualified Note.
                ldml_targets
                    .entry((decision, label))
                    .and_modify(|target| *target = None)
                    .or_insert(target);
            }
        }
        let mut updates = Vec::new();
        // A label's marks outside the notes, by opinion, in source order: where an opinion repeats a label,
        // its marks pair with its notes in order, the k-th mark with the k-th note, when they count the same.
        let in_note = |index: usize| {
            let mut parent = elements[index].parent;
            while let Some(index) = parent {
                if elements[index].tag == "footnote" {
                    return true;
                }
                parent = elements[index].parent;
            }
            false
        };
        let mut mark_counts = HashMap::<(usize, &str), usize>::new();
        for (index, element) in elements.iter().enumerate() {
            if element.tag != "footnotemark" || in_note(index) {
                continue;
            }
            let (Some(opinion), Some(raw)) = (opinion_owner(index), coordinates.slice_utf16(element.range.start..element.range.end)) else {
                continue;
            };
            *mark_counts.entry((opinion, trim_js(raw))).or_default() += 1;
        }
        let mut marks_seen = HashMap::<(usize, &str), usize>::new();
        for (index, element) in elements.iter().enumerate() {
            if ldml_reference(element) {
                if ["id", "name", "href", "anchoridref"]
                    .iter()
                    .any(|key| element.attributes.contains_key(*key))
                    || elements
                        .get(index + 1)
                        .is_some_and(|child| child.markup_start < element.markup_end)
                {
                    continue;
                }
                let Some(decision) = decision_owners[index] else {
                    continue;
                };
                let Some((writing, _)) = ldml_scope(decision) else {
                    continue;
                };
                if writing_owners[index] != Some(writing) {
                    continue;
                }
                let mut parent = element.parent;
                let mut blocked = false;
                while let Some(index) = parent {
                    let ancestor = &elements[index];
                    blocked |= matches!(
                        ancestor.tag.as_str(),
                        "blockquote" | "table" | "nav" | "script" | "style" | "a"
                    ) || [
                        "ldml-note",
                        "ldml-notes",
                        "ldml-referencenote",
                        "ldml-referencechain",
                        "ldml-refname",
                        "ldml-cite",
                        "ldml-headmatter",
                        "ldml-header",
                        "ldml-quotation",
                    ]
                    .iter()
                    .any(|class| has_class(ancestor, class));
                    parent = ancestor.parent;
                }
                if blocked {
                    continue;
                }
                let Some(label) = coordinates
                    .slice_utf16(element.range.start..element.range.end)
                    .filter(|label| courtlistener_ldml_bracketed_marker(label))
                else {
                    continue;
                };
                if let Some(note) = ldml_targets.get(&(decision, label)).copied().flatten() {
                    // This is an inferred printed-marker pair, not an explicit source link.
                    updates.push((note, element.range, None));
                }
                continue;
            }
            if element.tag == "footnote_reference"
                || element.tag.eq_ignore_ascii_case("footnotereference")
            {
                let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end)
                else {
                    continue;
                };
                let label = trim_js(raw);
                if label.is_empty() {
                    continue;
                }
                let target = if element.tag == "footnote_reference" {
                    matched_role_starts
                        .contains(&element.markup_start)
                        .then(|| numbered_targets.get(label).copied().flatten())
                        .flatten()
                } else {
                    element
                        .attributes
                        .get("anchoridref")
                        .and_then(|target| direct_targets.get(target.as_str()))
                        .copied()
                        .flatten()
                };
                let Some(note) = target else {
                    continue;
                };
                let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
                let start = element.range.start + utf16_len(&raw[..leading]);
                let anchor = element
                    .attributes
                    .get("id")
                    .filter(|id| identities.get(id.as_str()).copied().flatten() == Some(index))
                    .cloned();
                // IDREF and unique printed-marker relationships retain only
                // this reference's own actual unique ID as a fragment anchor.
                updates.push((
                    note,
                    ScalarRange {
                        start,
                        end: start + utf16_len(label),
                    },
                    anchor,
                ));
                continue;
            }
            if element.tag != "footnotemark" {
                continue;
            }
            let mut parent = element.parent;
            let mut in_note = false;
            while let Some(index) = parent {
                in_note |= elements[index].tag == "footnote";
                parent = elements[index].parent;
            }
            if in_note {
                continue;
            }
            let Some(opinion) = opinion_owner(index) else {
                continue;
            };
            let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end) else {
                continue;
            };
            let label = trim_js(raw);
            let ordinal = {
                let seen = marks_seen.entry((opinion, label)).or_default();
                *seen += 1;
                *seen - 1
            };
            let Some(owner) = owners.get(&(opinion, label)).and_then(|owned| match owned.len() {
                1 => Some(owned[0]),
                count if mark_counts.get(&(opinion, label)) == Some(&count) => owned.get(ordinal).copied(),
                _ => None,
            }) else {
                continue;
            };
            let Some(note) = nodes
                .get(&elements[owner].markup_start)
                .and_then(|node| notes.get(node))
            else {
                continue;
            };
            let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
            let start = element.range.start + utf16_len(&raw[..leading]);
            updates.push((
                *note,
                ScalarRange {
                    start,
                    end: start + utf16_len(label),
                },
                None,
            ));
        }
        updates
    };
    for (index, range, anchor) in updates {
        let note = &mut structure.notes[index];
        if !note
            .references
            .iter()
            .any(|reference| reference.range == range)
        {
            // IDREF markers retain only their own actual unique ID, if any.
            // Label-only same-opinion relationships supply no source fragment.
            note.references.push(NoteReference {
                range,
                anchor,
                page_indexes: Vec::new(),
                line_ids: Vec::new(),
            });
        }
        if note.primary_reference.is_none() {
            note.primary_reference = note.references.first().map(|reference| reference.range);
        }
    }
}

pub(super) fn clip_courtlistener_pre_note_bodies(structure: &mut DocumentStructure) {
    let ends = {
        let coordinates = ScalarText::new(structure.query_text());
        let pre = structure
            .markup_elements
            .iter()
            .filter(|element| element.tag == "pre")
            .collect::<Vec<_>>();
        let mut boundaries = Vec::new();
        for element in &pre {
            let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end) else {
                continue;
            };
            let mut start = element.range.start;
            let mut lines = raw.split_inclusive('\n').peekable();
            while let Some(line) = lines.next() {
                let value = line.trim_matches([' ', '\t', '\r', '\n']);
                let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                if (1..=3).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    let following = lines
                        .clone()
                        .find(|line| !line.trim_matches([' ', '\t', '\r', '\n']).is_empty());
                    if following.is_some_and(|line| {
                        !line.contains('\u{c}')
                            && line.len() - line.trim_start_matches([' ', '\t']).len() > indent
                            && line.chars().any(char::is_alphabetic)
                    }) {
                        boundaries.push((value.parse::<u32>().unwrap(), start, indent));
                    }
                }
                start += utf16_len(line);
            }
        }
        structure
            .notes
            .iter()
            .filter_map(|note| {
                let node = structure
                    .nodes
                    .iter()
                    .find(|node| note.node_id.as_deref() == Some(node.id.as_str()))?;
                if node.kind != NodeKind::Footnote
                    || node.source != Derivation::Heuristic
                    || note.primary_reference.is_none()
                {
                    return None;
                }
                let label =
                    coordinates.slice_utf16(note.label_range.start..note.label_range.end)?;
                let number = label.parse::<u32>().ok()?;
                let owner = pre.iter().find(|element| {
                    element.range.start <= note.label_range.start
                        && note.label_range.end <= element.range.end
                })?;
                let prefix = coordinates.slice_utf16(owner.range.start..note.label_range.start)?;
                let prefix = prefix.rsplit('\n').next()?;
                if !prefix.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
                    return None;
                }
                let end = boundaries
                    .iter()
                    .filter(|(next, start, indent)| {
                        *next == number + 1
                            && *indent == prefix.len()
                            && note.body_range.start < *start
                            && *start < note.body_range.end
                    })
                    .map(|(_, start, _)| *start)
                    .min()?;
                Some((note.node_id.clone()?, end))
            })
            .collect::<HashMap<_, _>>()
    };
    // PRE container starts can expose a printed next label even when HTML's
    // opening-LF removal joins it to the preceding rendered line. Bound only an
    // existing corroborated fragment; the next label supplies no new note/ref.
    for node in &mut structure.nodes {
        if let Some(end) = ends.get(&node.id) {
            node.range.end = *end;
        }
    }
    for note in &mut structure.notes {
        if let Some(end) = note.node_id.as_ref().and_then(|id| ends.get(id)) {
            note.body_range.end = *end;
            note.body_ranges.retain_mut(|range| {
                range.end = range.end.min(*end);
                range.start < range.end
            });
        }
    }
}

pub(super) fn supplement_courtlistener_bookmarked_notes(structure: &mut DocumentStructure) {
    if structure.notes.is_empty() {
        return;
    }
    let (updates, printed_updates) = {
        let elements = &structure.markup_elements;
        let has_class = |element: &MarkupElement, class: &str| {
            element
                .attributes
                .get("class")
                .is_some_and(|classes| classes.split_ascii_whitespace().any(|value| value == class))
        };
        let has_fn_notes = elements
            .iter()
            .any(|element| element.tag == "div" && has_class(element, "fn-footnote"));
        if !elements.iter().any(|element| {
            (element.tag.eq_ignore_ascii_case("a")
                && element
                    .attributes
                    .get("name")
                    .is_some_and(|name| name.starts_with("_ftn")))
                || (element.tag.eq_ignore_ascii_case("sup")
                    && element.attributes.get("id").is_some_and(|id| {
                        id.starts_with("fn") || id.starts_with("op") || id.starts_with("co_fnRef_")
                    }))
                || (element.tag.eq_ignore_ascii_case("span")
                    && has_class(element, "MsoFootnoteReference"))
                || (element.tag.eq_ignore_ascii_case("div")
                    && element
                        .attributes
                        .get("id")
                        .is_some_and(|id| id.starts_with("ftn")))
        }) && !has_fn_notes
        {
            return;
        }
        let reference_spans = elements
            .iter()
            .enumerate()
            .filter(|(_, element)| {
                element.tag.eq_ignore_ascii_case("sup")
                    || (element.tag.eq_ignore_ascii_case("span")
                        && has_class(element, "MsoFootnoteReference"))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut identities = HashMap::<&str, Option<usize>>::new();
        for (index, element) in elements.iter().enumerate() {
            for name in [
                element.attributes.get("id"),
                element
                    .tag
                    .eq_ignore_ascii_case("a")
                    .then(|| element.attributes.get("name"))
                    .flatten(),
            ]
            .into_iter()
            .flatten()
            .filter(|name| !name.is_empty())
            {
                identities
                    .entry(name.as_str())
                    .and_modify(|owner| {
                        if *owner != Some(index) {
                            *owner = None;
                        }
                    })
                    .or_insert(Some(index));
            }
        }
        let note_starts = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Footnote)
            .filter_map(|node| node.markup_start)
            .collect::<HashSet<_>>();
        let note_owner = |index: usize| {
            let mut parent = elements[index].parent;
            while let Some(index) = parent {
                let element = &elements[index];
                if note_starts.contains(&element.markup_start) {
                    return Some(element.markup_start);
                }
                parent = element.parent;
            }
            None
        };
        let parent_tag = |index: usize, tag: &str| {
            let mut parent = elements[index].parent;
            while let Some(index) = parent {
                if elements[index].tag.eq_ignore_ascii_case(tag) {
                    return Some(index);
                }
                parent = elements[index].parent;
            }
            None
        };
        let coordinates = ScalarText::new(structure.query_text());
        let printed_range = |index: usize, expected: &str| {
            let element = &elements[index];
            let raw = coordinates.slice_utf16(element.range.start..element.range.end)?;
            let printed = trim_js(raw);
            if printed != expected && printed != format!("[{expected}]") {
                return None;
            }
            let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
            let start = element.range.start + utf16_len(&raw[..leading]);
            Some(ScalarRange {
                start,
                end: start + utf16_len(printed),
            })
        };
        // Source ownership, rather than Word's formatting sections, bounds an
        // anonymous pair. A nested native opinion starts a distinct owner.
        let opinion_starts = structure
            .nodes
            .iter()
            .filter(|node| {
                node.source == Derivation::Native
                    && node.kind == NodeKind::Section
                    && (node.markup_tag.as_deref() == Some("opinion")
                        || node.markup_attributes.get("class").is_some_and(|classes| {
                            classes
                                .split_ascii_whitespace()
                                .any(|class| matches!(class, "co_opinionBlock" | "opinion"))
                        }))
            })
            .filter_map(|node| node.markup_start)
            .collect::<HashSet<_>>();
        let mut document_owners = Vec::with_capacity(elements.len());
        for (index, element) in elements.iter().enumerate() {
            document_owners.push(if opinion_starts.contains(&element.markup_start) {
                index
            } else {
                element
                    .parent
                    .map_or(index, |parent| document_owners[parent])
            });
        }
        let anonymous_markers = reference_spans
            .iter()
            .copied()
            .filter(|&index| {
                let element = &elements[index];
                let superscript = element.tag.eq_ignore_ascii_case("sup");
                if !(superscript
                    || (element.tag.eq_ignore_ascii_case("span")
                        && has_class(element, "MsoFootnoteReference")))
                    || parent_tag(index, "a").is_some()
                {
                    return false;
                }
                let mut ancestor = element.parent;
                while let Some(index) = ancestor {
                    if has_class(&elements[index], "MsoFootnoteReference")
                        || (superscript && elements[index].tag.eq_ignore_ascii_case("sup"))
                    {
                        return false;
                    }
                    ancestor = elements[index].parent;
                }
                std::iter::once(element)
                    .chain(
                        elements[index + 1..]
                            .iter()
                            .take_while(|child| child.markup_start < element.markup_end),
                    )
                    .enumerate()
                    .all(|(position, child)| {
                        !child.tag.eq_ignore_ascii_case("a")
                            && !["id", "href", "name"]
                                .iter()
                                .any(|key| child.attributes.contains_key(*key))
                            && (!superscript
                                || position == 0
                                || (!child.tag.eq_ignore_ascii_case("sup")
                                    && !has_class(child, "MsoFootnoteReference")))
                    })
            })
            .collect::<Vec<_>>();
        let mut native_body_counts = HashMap::<(usize, &str), usize>::new();
        for node in structure
            .nodes
            .iter()
            .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote)
        {
            if let Some((index, label)) = node.markup_start.and_then(|start| {
                Some((
                    elements
                        .binary_search_by_key(&start, |element| element.markup_start)
                        .ok()?,
                    node.label.as_deref()?,
                ))
            }) {
                *native_body_counts
                    .entry((document_owners[index], label))
                    .or_default() += 1;
            }
        }
        let mut updates = Vec::new();
        let mut printed_updates = Vec::new();
        let sibling_markers = courtlistener_sibling_note_markers(elements, structure.query_text());
        let note_nodes = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Footnote)
            .map(|node| (node.id.as_str(), node))
            .collect::<HashMap<_, _>>();
        let fn_writing_starts = if has_fn_notes {
            structure
                .nodes
                .iter()
                .filter(|node| {
                    node.source == Derivation::Native
                        && node.kind == NodeKind::Section
                        && (node
                            .markup_start
                            .is_some_and(|start| opinion_starts.contains(&start))
                            || matches!(
                                node.markup_tag.as_deref(),
                                Some("opinion_text" | "concurrence_text" | "dissent_text")
                            )
                            || node.markup_attributes.get("class").is_some_and(|classes| {
                                classes
                                    .split_ascii_whitespace()
                                    .any(|class| matches!(class, "ldml-opinion" | "de-writing"))
                            }))
                })
                .filter_map(|node| node.markup_start)
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let mut fn_writings = Vec::new();
        if has_fn_notes {
            for (index, element) in elements.iter().enumerate() {
                fn_writings.push(if fn_writing_starts.contains(&element.markup_start) {
                    Some(index)
                } else {
                    element.parent.and_then(|parent| fn_writings[parent])
                });
            }
        }
        let numeric_marker = |index: usize| {
            let element = &elements[index];
            if element.tag != "sup" {
                return None;
            }
            let raw = coordinates.slice_utf16(element.range.start..element.range.end)?;
            let printed = trim_js(raw);
            if printed.is_empty()
                || printed.len() > 5
                || !printed.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            Some((
                footnote_identity(printed, "", None)?,
                printed_range(index, printed)?,
                printed,
            ))
        };
        let fn_nodes = note_nodes
            .values()
            .filter(|node| {
                has_fn_notes
                    && node.source == Derivation::Native
                    && node.markup_tag.as_deref() == Some("div")
                    && node.markup_attributes.get("class").is_some_and(|classes| {
                        classes
                            .split_ascii_whitespace()
                            .any(|class| class == "fn-footnote")
                    })
            })
            .filter_map(|node| Some((node.markup_start?, *node)))
            .collect::<HashMap<_, _>>();
        let mut fn_markers = HashMap::new();
        let fn_marker_sources = if has_fn_notes {
            anonymous_markers.as_slice()
        } else {
            &[]
        };
        for index in fn_marker_sources.iter().copied() {
            let Some(owner) = note_owner(index) else {
                continue;
            };
            let Some(node) = fn_nodes.get(&owner) else {
                continue;
            };
            let Some((identity, range, printed)) = numeric_marker(index) else {
                continue;
            };
            if !coordinates
                .slice_utf16(node.range.start..range.start)
                .is_some_and(|prefix| trim_js(prefix).is_empty())
                || (node.marker_range.is_some() && node.marker_range != Some(range))
                || ["data-label", "label", "n"].iter().any(|key| {
                    node.markup_attributes
                        .get(*key)
                        .filter(|raw| !trim_js(raw).is_empty())
                        .is_some_and(|raw| {
                            footnote_identity(raw, "", None)
                                .is_none_or(|declared| declared.label != identity.label)
                        })
                })
            {
                continue;
            }
            fn_markers
                .entry(owner)
                .and_modify(|marker| *marker = None)
                .or_insert(Some((identity, range, printed)));
        }
        let mut fn_body_counts = HashMap::new();
        for node in note_nodes
            .values()
            .filter(|node| has_fn_notes && node.source == Derivation::Native)
        {
            if let Some((index, label)) = node.markup_start.and_then(|start| {
                Some((
                    elements
                        .binary_search_by_key(&start, |element| element.markup_start)
                        .ok()?,
                    fn_markers
                        .get(&start)
                        .and_then(Option::as_ref)
                        .map_or_else(
                            || node.label.as_deref(),
                            |(identity, _, _)| Some(identity.label.as_str()),
                        )?,
                ))
            }) {
                *fn_body_counts
                    .entry((fn_writings[index], label))
                    .or_insert(0usize) += 1;
            }
        }
        for (note_index, note) in structure.notes.iter().enumerate() {
            let Some(&node) = note.node_id.as_deref().and_then(|id| note_nodes.get(id)) else {
                continue;
            };
            if let Some((identity, marker, printed)) = node
                .markup_start
                .and_then(|start| fn_markers.get(&start))
                .and_then(Option::as_ref)
            {
                let Some(body_index) = node.markup_start.and_then(|start| {
                    elements
                        .binary_search_by_key(&start, |element| element.markup_start)
                        .ok()
                }) else {
                    continue;
                };
                let label = identity.label.as_str();
                if note.label_range.start != note.label_range.end && note.label_range != *marker {
                    continue;
                }
                let mut references = Vec::new();
                if note.references.is_empty()
                    && note.primary_reference.is_none()
                    && fn_body_counts.get(&(fn_writings[body_index], label)) == Some(&1)
                {
                    let mut container = elements[body_index].parent;
                    while container
                        .is_some_and(|index| !has_class(&elements[index], "fn-container"))
                    {
                        container = elements[container.unwrap()].parent;
                    }
                    if let Some(container) = container {
                        for index in anonymous_markers.iter().copied() {
                            if elements[index].markup_end > elements[container].markup_start
                                || note_owner(index).is_some()
                                || fn_writings[index] != fn_writings[body_index]
                            {
                                continue;
                            }
                            // Without a supplied writing, only its actual enclosing
                            // source container corroborates an anonymous reference.
                            if fn_writings[body_index].is_none() {
                                let Some(parent) = elements[container].parent else {
                                    continue;
                                };
                                let mut ancestor = elements[index].parent;
                                while ancestor.is_some_and(|index| index != parent) {
                                    ancestor = elements[ancestor.unwrap()].parent;
                                }
                                if ancestor != Some(parent) {
                                    continue;
                                }
                            }
                            if let Some((_, range, _)) = numeric_marker(index)
                                .filter(|(reference, _, _)| reference.label == label)
                            {
                                references.push(NoteReference {
                                    range,
                                    anchor: None,
                                    page_indexes: Vec::new(),
                                    line_ids: Vec::new(),
                                });
                            }
                        }
                    }
                }
                printed_updates.push((
                    note_index,
                    *marker,
                    label.to_owned(),
                    (*printed).to_owned(),
                    references,
                ));
                continue;
            }
            if let Some((label, range)) = node
                .markup_start
                .and_then(|start| sibling_markers.get(&start))
            {
                if node.label.as_ref() != Some(label)
                    || note.label_range != *range
                    || !note.references.is_empty()
                    || note.primary_reference.is_some()
                {
                    continue;
                }
                let Some(body_index) = elements
                    .binary_search_by_key(&node.markup_start.unwrap(), |element| {
                        element.markup_start
                    })
                    .ok()
                else {
                    continue;
                };
                let Some(parent) = elements[body_index].parent else {
                    continue;
                };
                let number = label.strip_prefix("fn").unwrap();
                let mut markers = elements.iter().enumerate().filter_map(|(index, element)| {
                    let number_index = element.parent?;
                    let number_element = &elements[number_index];
                    let id = element.attributes.get("id")?;
                    let opaque = id.strip_prefix("co_footnote_")?;
                    (element.tag == "span"
                        && !opaque.is_empty()
                        && number_element.tag == "div"
                        && has_class(number_element, "co_footnoteNumber")
                        && number_element.parent == Some(parent)
                        && identities.get(id.as_str()).copied().flatten() == Some(index)
                        && printed_range(index, number) == Some(*range))
                    .then_some(opaque)
                });
                let Some(opaque) = markers.next() else {
                    continue;
                };
                if markers.next().is_some() {
                    continue;
                }
                let prefix = format!("co_fnRef_{opaque}_ID");
                let mut references = reference_spans.iter().filter_map(|&index| {
                    let reference = &elements[index];
                    let id = reference.attributes.get("id")?;
                    let suffix = id.strip_prefix(&prefix)?;
                    let range = printed_range(index, number)?;
                    (reference.tag == "sup"
                        && !suffix.is_empty()
                        && identities.get(id.as_str()).copied().flatten() == Some(index)
                        && note_owner(index).is_none()
                        && reference.markup_end <= node.markup_start?)
                        .then_some((range, Some(id.clone())))
                });
                let reference = references.next();
                if references.next().is_none() {
                    if let Some(reference) = reference {
                        // The literal opaque identity links the superscript to
                        // its sibling label, without inventing an href or body ID.
                        updates.push((note_index, None, Some(reference)));
                    }
                }
                continue;
            }
            if note.label_range.start != note.label_range.end {
                continue;
            }
            if node.anchor.is_none()
                && (node.markup_tag.as_deref() == Some("footnote_body")
                    || (node.markup_tag.as_deref() == Some("div")
                        && node.markup_attributes.get("class").is_some_and(|classes| {
                            classes
                                .split_ascii_whitespace()
                                .any(|class| class == "footnote")
                        })))
            {
                let Some(label_id) = node.label.as_deref() else {
                    continue;
                };
                let Some(number) = label_id.strip_prefix("fn").filter(|number| {
                    *number == "*"
                        || (!number.is_empty()
                            && number.len() <= 5
                            && number.bytes().all(|byte| byte.is_ascii_digit()))
                }) else {
                    continue;
                };
                let mut markers = elements.iter().enumerate().filter_map(|(index, element)| {
                    let id = element.attributes.get("id")?;
                    let scoped_label = id
                        .strip_prefix("op")
                        .and_then(|value| value.split_once('-'));
                    (element.tag == "sup"
                        && (id == label_id
                            || scoped_label.is_some_and(|(opinion, label)| {
                                node.markup_tag.as_deref() == Some("div")
                                    && !opinion.is_empty()
                                    && opinion.bytes().all(|byte| byte.is_ascii_digit())
                                    && label == label_id
                            }))
                        && identities.get(id.as_str()).copied().flatten() == Some(index)
                        && note_owner(index) == node.markup_start
                        && printed_range(index, number)
                            .is_some_and(|range| range.start == note.body_range.start))
                    .then_some(index)
                });
                let Some(marker_index) = markers.next() else {
                    continue;
                };
                if markers.next().is_some() {
                    continue;
                }
                let marker = &elements[marker_index];
                let Some(label_range) = printed_range(marker_index, number) else {
                    continue;
                };
                if !marker.tag.eq_ignore_ascii_case("sup")
                    || node.markup_start.is_none()
                    || note_owner(marker_index) != node.markup_start
                    || label_range.start != note.body_range.start
                    || !coordinates
                        .slice_utf16(label_range.end..note.body_range.end)
                        .is_some_and(|body| body.chars().any(char::is_alphabetic))
                {
                    continue;
                }
                updates.push((note_index, Some(label_range), None));
                if !note.references.is_empty() || note.primary_reference.is_some() {
                    continue;
                }
                let forward_href = format!("#{}", marker.attributes["id"]);
                if elements
                    .iter()
                    .filter(|element| {
                        element.tag.eq_ignore_ascii_case("a")
                            && element.attributes.get("href") == Some(&forward_href)
                    })
                    .count()
                    != 1
                {
                    continue;
                }
                let mut pairs = elements.iter().enumerate().filter_map(|(index, backlink)| {
                    if !backlink.tag.eq_ignore_ascii_case("a")
                        || backlink.parent != Some(marker_index)
                        || printed_range(index, number) != Some(label_range)
                    {
                        return None;
                    }
                    let reference_id = backlink.attributes.get("href")?.strip_prefix('#')?;
                    let reference_index = identities.get(reference_id).copied().flatten()?;
                    let reference = &elements[reference_index];
                    let range = printed_range(reference_index, number)?;
                    if !reference.tag.eq_ignore_ascii_case("sup")
                        || note_owner(reference_index).is_some()
                        || reference.markup_end > node.markup_start?
                        || elements
                            .iter()
                            .enumerate()
                            .filter(|(index, element)| {
                                element.tag.eq_ignore_ascii_case("a")
                                    && element.parent == Some(reference_index)
                                    && element.attributes.get("href") == Some(&forward_href)
                                    && printed_range(*index, number) == Some(range)
                            })
                            .count()
                            != 1
                    {
                        return None;
                    }
                    Some((range, Some(reference_id.to_owned())))
                });
                let pair = pairs.next();
                if pairs.next().is_none() {
                    // IDs belong to the actual superscripts, not the idless
                    // note container. Preserve that source ownership while
                    // adding the reciprocal semantic reference.
                    updates.last_mut().unwrap().2 = pair;
                }
                continue;
            }
            let Some(body_id) = node.anchor.as_deref() else {
                continue;
            };
            let Some(number) = body_id.strip_prefix("ftn").filter(|number| {
                !number.is_empty()
                    && number.len() <= 5
                    && number.bytes().all(|byte| byte.is_ascii_digit())
            }) else {
                continue;
            };
            let Some(owner_index) = identities.get(body_id).copied().flatten() else {
                continue;
            };
            let owner = &elements[owner_index];
            if !owner.tag.eq_ignore_ascii_case("div")
                || node.markup_start != Some(owner.markup_start)
            {
                continue;
            }
            let body_name = format!("_{body_id}");
            let reference_name = format!("_ftnref{number}");
            let owner_scope = document_owners[owner_index];
            if node.source == Derivation::Native
                && node
                    .label
                    .as_deref()
                    .and_then(|label| label.strip_prefix("fn"))
                    == Some(number)
                && owner.attributes.get("id").map(String::as_str) == Some(body_id)
                && !identities.contains_key(body_name.as_str())
                && !identities.contains_key(reference_name.as_str())
                && node
                    .label
                    .as_deref()
                    .is_some_and(|label| native_body_counts.get(&(owner_scope, label)) == Some(&1))
            {
                let printed = format!("[{number}]");
                let marker_range = |index| {
                    let range = printed_range(index, number)?;
                    (coordinates.slice_utf16(range.start..range.end) == Some(printed.as_str()))
                        .then_some(range)
                };
                let mut labels = anonymous_markers.iter().filter_map(|&index| {
                    let range = printed_range(index, number)?;
                    let printed_label = if elements[index].tag.eq_ignore_ascii_case("sup") {
                        number
                    } else {
                        printed.as_str()
                    };
                    (coordinates.slice_utf16(range.start..range.end) == Some(printed_label)
                        && note_owner(index) == Some(owner.markup_start)
                        && document_owners[index] == owner_scope
                        && range.start == note.body_range.start)
                        .then_some(range)
                });
                if let Some(label_range) = labels.next().filter(|range| {
                    coordinates
                        .slice_utf16(range.end..note.body_range.end)
                        .is_some_and(|body| body.chars().any(char::is_alphabetic))
                }) {
                    if labels.next().is_some() {
                        continue;
                    }
                    // A printed label is independent of whether the
                    // corresponding reference is absent or already known.
                    updates.push((note_index, Some(label_range), None));
                    if !note.references.is_empty() || note.primary_reference.is_some() {
                        continue;
                    }
                    let mut references = anonymous_markers.iter().filter_map(|&index| {
                        if !elements[index].tag.eq_ignore_ascii_case("span")
                            || !has_class(&elements[index], "MsoFootnoteReference")
                        {
                            return None;
                        }
                        let range = marker_range(index)?;
                        (note_owner(index).is_none() && document_owners[index] == owner_scope)
                            .then_some((index, range))
                    });
                    if let Some((index, range)) = references.next() {
                        if references.next().is_none()
                            && elements[index].markup_end <= owner.markup_start
                        {
                            // The semantic pair has no source fragment.
                            updates.last_mut().unwrap().2 = Some((range, None));
                        }
                    }
                    continue;
                }
            }
            let Some(body_bookmark_index) = identities.get(body_name.as_str()).copied().flatten()
            else {
                continue;
            };
            let body_bookmark = &elements[body_bookmark_index];
            if !body_bookmark.tag.eq_ignore_ascii_case("a")
                || body_bookmark.range.start != body_bookmark.range.end
                || body_bookmark.attributes.contains_key("href")
                || body_bookmark.attributes.get("name") != Some(&body_name)
                || note_owner(body_bookmark_index) != Some(owner.markup_start)
            {
                continue;
            }
            let Some(paragraph) = body_bookmark.parent else {
                continue;
            };
            if !elements[paragraph].tag.eq_ignore_ascii_case("p") {
                continue;
            }
            let printed = number;
            let mut labels = elements.iter().enumerate().filter_map(|(index, element)| {
                if !element.tag.eq_ignore_ascii_case("a")
                    || element.parent != Some(paragraph)
                    || note_owner(index) != Some(owner.markup_start)
                {
                    return None;
                }
                let mut superscripts =
                    elements.iter().enumerate().filter_map(|(sup_index, sup)| {
                        (sup.tag.eq_ignore_ascii_case("sup")
                            && parent_tag(sup_index, "a") == Some(index))
                        .then(|| printed_range(sup_index, printed))
                        .flatten()
                        .filter(|range| range.start == note.body_range.start)
                    });
                let superscript = superscripts.next();
                if superscripts.next().is_some() {
                    return None;
                }
                let range = superscript.or_else(|| {
                    has_class(&elements[paragraph], "MsoFootnoteText")
                        .then(|| printed_range(index, printed))
                        .flatten()
                })?;
                Some((index, element, range))
            });
            let Some((label_index, label, label_range)) = labels.next() else {
                continue;
            };
            if labels.next().is_some() {
                continue;
            }
            if body_bookmark.markup_end > label.markup_start
                || body_bookmark.range.start != label_range.start
                || label_range.start != note.body_range.start
                || !coordinates
                    .slice_utf16(label_range.end..note.body_range.end)
                    .is_some_and(|body| body.chars().any(char::is_alphabetic))
            {
                continue;
            }
            // The leading declared label is independent of a reference's
            // printed text or href. Keep a conflict without losing the label.
            updates.push((note_index, Some(label_range), None));
            if !note.references.is_empty() || note.primary_reference.is_some() {
                continue;
            }
            let Some(reference_bookmark_index) =
                identities.get(reference_name.as_str()).copied().flatten()
            else {
                continue;
            };
            let reference_bookmark = &elements[reference_bookmark_index];
            if !reference_bookmark.tag.eq_ignore_ascii_case("a")
                || reference_bookmark.range.start != reference_bookmark.range.end
                || reference_bookmark.attributes.contains_key("href")
                || reference_bookmark.attributes.get("name") != Some(&reference_name)
                || note_owner(reference_bookmark_index).is_some()
            {
                continue;
            }
            let Some(reference_paragraph) = parent_tag(reference_bookmark_index, "p") else {
                continue;
            };
            let Some((prefix, target)) = label
                .attributes
                .get("href")
                .and_then(|href| href.rsplit_once('#'))
            else {
                continue;
            };
            if prefix.is_empty() || target != reference_name {
                continue;
            }
            if printed_range(label_index, printed) != Some(label_range) {
                continue;
            }
            let mut matches = reference_spans
                .iter()
                .filter_map(|&span_index| {
                    if note_owner(span_index).is_some() {
                        return None;
                    }
                    let reference_index = parent_tag(span_index, "a")?;
                    let reference = &elements[reference_index];
                    let range = printed_range(span_index, &printed)?;
                    (reference
                        .attributes
                        .get("href")
                        .and_then(|href| href.rsplit_once('#'))
                        == Some((prefix, body_name.as_str()))
                        && printed_range(reference_index, &printed) == Some(range)
                        && reference_bookmark.markup_end <= reference.markup_start
                        && reference.markup_end <= owner.markup_start
                        && reference_bookmark.range.start == range.start
                        && parent_tag(reference_index, "p") == Some(reference_paragraph))
                    .then_some((reference_index, range))
                })
                .collect::<Vec<_>>();
            matches.sort_unstable_by_key(|(index, range)| (*index, range.start, range.end));
            matches.dedup();
            if matches.len() != 1 {
                continue;
            }
            // Reciprocal bookmarks support the semantic pair without claiming
            // that the preserved absolute href is the current source URI.
            updates.last_mut().unwrap().2 = Some((matches[0].1, Some(reference_name)));
        }
        (updates, printed_updates)
    };
    for (index, label_range, reference) in updates {
        let note = &mut structure.notes[index];
        if let Some(label_range) = label_range {
            note.label_range = label_range;
            note.label_ranges = vec![label_range];
            note.body_range.start = label_range.end;
            note.body_ranges.retain_mut(|range| {
                range.start = range.start.max(label_range.end);
                range.start < range.end
            });
        }
        if let Some((reference_range, reference_name)) = reference {
            note.primary_reference = Some(reference_range);
            note.references.push(NoteReference {
                range: reference_range,
                anchor: reference_name,
                page_indexes: Vec::new(),
                line_ids: Vec::new(),
            });
        }
    }
    let mut markers = HashMap::new();
    for (index, marker, label, printed, references) in printed_updates {
        let note = &mut structure.notes[index];
        note.label_range = marker;
        note.label_ranges = vec![marker];
        if let Some(id) = &note.node_id { markers.insert(id.clone(), (marker, label, printed)); }
        if !references.is_empty() {
            note.primary_reference = references.first().map(|reference| reference.range);
            note.references = references;
        }
    }
    // The body is already supplied. Attach its printed marker/reference
    // evidence without moving its range, identity or existing stronger links.
    if !markers.is_empty() {
        for node in &mut structure.nodes {
            if let Some((marker, label, printed)) = markers.get(&node.id) {
                node.marker_range = Some(*marker);
                node.label = Some(label.clone());
                let aliases = node.aliases.get_or_insert_with(Vec::new);
                for alias in [printed.clone(), format!("footnote {printed}")] {
                    if !aliases.contains(&alias) {
                        aliases.push(alias);
                    }
                }
            }
        }
    }
}

pub(super) fn courtlistener_declared_note_marker(
    elements: &[MarkupElement],
    index: usize,
    text: &ScalarText,
    owners: &HashMap<usize, (Kind, usize, usize)>,
) -> Option<(PendingBlock, ScalarRange)> {
    let note = &elements[index];
    let superscript = elements[index + 1..]
        .iter()
        .take_while(|element| element.markup_start < note.markup_end)
        .find(|element| {
            if !element.tag.eq_ignore_ascii_case("sup") {
                return false;
            }
            let mut parent = element.parent;
            while let Some(ancestor) = parent {
                if ancestor == index {
                    return true;
                }
                let element = &elements[ancestor];
                if courtlistener_ldml_note(element)
                    || owners
                        .get(&element.markup_start)
                        .is_some_and(|(kind, _, _)| *kind == Kind::Footnote)
                {
                    return false;
                }
                parent = element.parent;
            }
            false
        })?;
    let printed = text.slice_utf16(superscript.range.start..superscript.range.end)?;
    let printed = trim_js(printed);
    let (marker, start) = if printed.is_empty() {
        let raw = text.slice_utf16(note.range.start..note.range.end)?;
        let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
        let start = note.range.start + utf16_len(&raw[..leading]);
        if superscript.range.end > start {
            return None;
        }
        let raw = &raw[leading..];
        let digits = raw.bytes().take(6).take_while(u8::is_ascii_digit).count();
        let marker = if (1..=5).contains(&digits)
            && raw[digits..].starts_with('.')
            && raw[digits + 1..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_ascii_digit())
        {
            // LDML also prints N. immediately after an empty SUP, sometimes
            // without separator whitespace. Keep the printed punctuation.
            &raw[..digits + 1]
        } else {
            leading_footnote_marker(raw)?
        };
        (marker, start)
    } else {
        if leading_footnote_marker(printed) != Some(printed) {
            return None;
        }
        let raw = text.slice_utf16(superscript.range.start..superscript.range.end)?;
        let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
        (
            printed,
            superscript.range.start + utf16_len(&raw[..leading]),
        )
    };
    if !trim_js(text.slice_utf16(note.range.start..start)?).is_empty() {
        return None;
    }
    let marker_range = ScalarRange {
        start,
        end: start + utf16_len(marker),
    };
    if elements[index + 1..]
        .iter()
        .take_while(|element| element.markup_start < note.markup_end)
        .any(|element| {
            (courtlistener_ldml_note(element)
                || owners
                    .get(&element.markup_start)
                    .is_some_and(|(kind, _, _)| *kind == Kind::Footnote))
                && element.range.start < marker_range.end
                && marker_range.start < element.range.end
        })
    {
        return None;
    }
    let mut identity = footnote_identity(marker.strip_suffix('.').unwrap_or(marker), "", None)?;
    if marker.ends_with('.') {
        identity.aliases = vec![marker.to_owned(), format!("footnote {marker}")];
    }
    Some((identity, marker_range))
}

pub(super) fn courtlistener_declared_blocks(
    elements: &[MarkupElement],
    existing: &[RawBlock],
    recovered_bodies: &[RawBlock],
    matched_role_starts: &HashSet<usize>,
    text: &str,
) -> Vec<RawBlock> {
    let declared_role = |element: &MarkupElement| {
        let class = |name: &str| {
            element
                .attributes
                .get("class")
                .is_some_and(|classes| classes.split_ascii_whitespace().any(|value| value == name))
        };
        if courtlistener_ldml_note(element) && matched_role_starts.contains(&element.markup_start) {
            Some(Kind::Footnote)
        } else if (element.tag.eq_ignore_ascii_case("div")
            && (class("ldml-opinion") || class("ldml-section")))
            || (element.tag.eq_ignore_ascii_case("span") && class("ldml-appendix"))
            || (element.tag.eq_ignore_ascii_case("div")
                && class("de-writing")
                && matched_role_starts.contains(&element.markup_start))
            || (element.tag.eq_ignore_ascii_case("summary")
                && matched_role_starts.contains(&element.markup_start)
                && element.range.start < element.range.end
                && element
                    .parent
                    .is_some_and(|parent| elements[parent].tag.eq_ignore_ascii_case("opinion")))
            || (matches!(
                element.tag.as_str(),
                "opinion_text"
                    | "concurrence_text"
                    | "dissent_text"
                    | "appendix"
                    | "correction"
                    | "syllabus"
            ) && matched_role_starts.contains(&element.markup_start))
        {
            Some(Kind::Section)
        } else if (element.tag.eq_ignore_ascii_case("section") && class("ldml-heading"))
            || (element.tag == "p"
                && element
                    .attributes
                    .get("data-type")
                    .is_some_and(|role| role == "header")
                && matched_role_starts.contains(&element.markup_start))
        {
            Some(Kind::Heading)
        } else {
            None
        }
    };
    if !elements
        .iter()
        .any(|element| element.tag.eq_ignore_ascii_case("dl") || declared_role(element).is_some())
    {
        return Vec::new();
    }
    let note_text = elements
        .iter()
        .any(|element| declared_role(element) == Some(Kind::Footnote))
        .then(|| ScalarText::new(text));
    let mut owners = existing
        .iter()
        .chain(recovered_bodies)
        .filter_map(|block| Some((block.metadata.start?, (block.kind, block.start, block.end))))
        .collect::<HashMap<_, _>>();
    let mut recovered = Vec::new();
    for (index, element) in elements.iter().enumerate() {
        let list = element.tag.eq_ignore_ascii_case("dl");
        let item = (element.tag.eq_ignore_ascii_case("dt")
            || element.tag.eq_ignore_ascii_case("dd"))
            && element.parent.is_some_and(|parent| {
                let owner = &elements[parent];
                owner.tag.eq_ignore_ascii_case("dl")
                    && owners
                        .get(&owner.markup_start)
                        .is_some_and(|&(kind, _, _)| kind == Kind::List)
            });
        let kind = if let Some(kind) = declared_role(element) {
            kind
        } else if list || item {
            Kind::List
        } else {
            continue;
        };
        if owners.contains_key(&element.markup_start) {
            continue;
        }
        let mut parent = element.parent;
        let mut parent_label = None;
        while let Some(index) = parent {
            let ancestor = &elements[index];
            if owners
                .get(&ancestor.markup_start)
                .is_some_and(|&(kind, start, end)| {
                    kind != Kind::Page && start <= element.range.start && end >= element.range.end
                })
            {
                parent_label = Some(format!("native-markup-{}", ancestor.markup_start));
                break;
            }
            parent = ancestor.parent;
        }
        let attributes = element
            .attributes
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let anchor = attributes.get("id").filter(|id| !id.is_empty()).cloned();
        let note = (kind == Kind::Footnote)
            .then(|| {
                courtlistener_declared_note_marker(elements, index, note_text.as_ref()?, &owners)
            })
            .flatten();
        let note_marker = note.as_ref().map(|(_, range)| *range);
        let (label, aliases) = note.map_or_else(
            || {
                (
                    if kind == Kind::Footnote {
                        String::new()
                    } else {
                        anchor.clone().unwrap_or_default()
                    },
                    Vec::new(),
                )
            },
            |(identity, _)| (identity.label, identity.aliases),
        );
        recovered.push(RawBlock {
            metadata: MarkupMetadata {
                start: Some(element.markup_start),
                attributes,
                note_marker,
            },
            kind,
            label,
            start: note_marker.map_or(element.range.start, |marker| marker.end),
            end: element.range.end,
            anchor,
            markup_tag: Some(element.tag.to_ascii_lowercase()),
            aliases,
            parent_label,
        });
        owners.insert(
            element.markup_start,
            (kind, element.range.start, element.range.end),
        );
    }
    recovered
}

pub(super) fn courtlistener_printed_note_blocks(
    markup: &str,
    elements: &[MarkupElement],
    text: &ScalarText<'_>,
    existing: &[RawBlock],
) -> Vec<(RawBlock, Vec<NoteReference>)> {
    if !elements.iter().any(|element| {
        element.tag == "h2"
            && text
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
                == Some("NOTES")
    }) {
        return Vec::new();
    }
    let existing_notes = existing
        .iter()
        .filter(|block| block.kind == Kind::Footnote)
        .map(|block| ScalarRange {
            start: block.start,
            end: block.end,
        })
        .collect::<Vec<_>>();
    let matched = |element: &MarkupElement| {
        markup
            .get(element.markup_start..element.markup_end)
            .is_some_and(|raw| {
                raw.rfind("</")
                    .and_then(|end| parse_tag(&raw[end..], true))
                    .is_some_and(|(tag, _)| tag == element.tag)
            })
    };
    let mut writings = Vec::with_capacity(elements.len());
    for (index, element) in elements.iter().enumerate() {
        let writing = matches!(
            element.tag.as_str(),
            "opinion" | "opinion_text" | "concurrence_text" | "dissent_text"
        ) || (matches!(element.tag.as_str(), "article" | "section" | "div")
            && element.attributes.get("class").is_some_and(|classes| {
                classes.split_ascii_whitespace().any(|class| {
                    class == "opinion"
                        || (element.tag == "div"
                            && matches!(class, "ldml-opinion" | "de-writing" | "co_opinionBlock"))
                })
            }));
        writings.push(if writing {
            Some(index)
        } else {
            element.parent.and_then(|parent| writings[parent])
        });
    }
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^(\[(?:[0-9]{1,5}|\*{1,2})\])(?:\s|$)").unwrap());
    let printed = |element: &MarkupElement| {
        let raw = text.slice_utf16(element.range.start..element.range.end)?;
        let value = raw.trim_start_matches(javascript_whitespace);
        let marker = label.captures(value)?.get(1)?;
        let start = element.range.start + utf16_len(&raw[..raw.len() - value.len()]);
        Some((
            marker.as_str(),
            ScalarRange {
                start,
                end: start + utf16_len(marker.as_str()),
            },
        ))
    };
    let mut result = Vec::new();
    let mut previous_ends = HashMap::new();
    for (heading_index, heading) in elements.iter().enumerate().filter(|(_, element)| {
        element.tag == "h2"
            && matched(element)
            && text
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
                == Some("NOTES")
    }) {
        let Some(parent) = heading.parent.filter(|&parent| {
            matches!(elements[parent].tag.as_str(), "div" | "pre") && matched(&elements[parent])
        }) else {
            continue;
        };
        let writing = writings[heading_index];
        let boundary = elements[heading_index + 1..]
            .iter()
            .enumerate()
            .take_while(|(_, element)| element.markup_start < elements[parent].markup_end)
            .find(|(index, element)| {
                if element.parent == Some(parent) && matches!(element.tag.as_str(), "h1" | "h2") {
                    return true;
                }
                let index = heading_index + 1 + *index;
                if writings[index] != Some(index) || writings[index] == writing {
                    return false;
                }
                // An embedded quoted writing is continuation evidence, not a
                // new sibling writing after the notes region.
                let mut ancestor = element.parent;
                while let Some(index) = ancestor {
                    if index == parent {
                        return true;
                    }
                    if matches!(elements[index].tag.as_str(), "p" | "blockquote" | "table") {
                        return false;
                    }
                    ancestor = elements[index].parent;
                }
                false
            })
            .map(|(_, element)| element);
        let source_end =
            boundary.map_or(elements[parent].markup_end, |element| element.markup_start);
        let rendered_end =
            boundary.map_or(elements[parent].range.end, |element| element.range.start);
        let lower_bound = previous_ends
            .insert((parent, writing), source_end)
            .unwrap_or(elements[parent].markup_start);
        let bodies = elements[heading_index + 1..]
            .iter()
            .enumerate()
            .take_while(|(_, element)| element.markup_start < source_end)
            .filter(|(index, element)| {
                element.tag == "p"
                    && element.parent == Some(parent)
                    && writings[heading_index + 1 + *index] == writing
                    && matched(element)
            })
            .filter_map(|(_, element)| {
                printed(element).map(|(label, marker)| (element, label, marker))
            })
            .collect::<Vec<_>>();
        if bodies.is_empty() {
            continue;
        }
        let mut references = HashMap::<&str, Vec<NoteReference>>::new();
        for (index, element) in
            elements[..heading_index]
                .iter()
                .enumerate()
                .filter(|(_, element)| {
                    element.tag == "sup" && element.markup_start >= lower_bound && matched(element)
                })
        {
            if writings[index] != writing {
                continue;
            }
            let mut ancestor = element.parent;
            while let Some(index) = ancestor {
                if index == parent {
                    break;
                }
                ancestor = elements[index].parent;
            }
            if ancestor != Some(parent) {
                continue;
            }
            let Some((marker, range)) = printed(element) else {
                continue;
            };
            if text
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
                != Some(marker)
                || existing_notes
                    .iter()
                    .any(|note| note.start <= range.start && range.end <= note.end)
            {
                continue;
            }
            references.entry(marker).or_default().push(NoteReference {
                range,
                anchor: None,
                page_indexes: Vec::new(),
                line_ids: Vec::new(),
            });
        }
        let mut counts = HashMap::new();
        for (_, marker, _) in &bodies {
            *counts.entry(*marker).or_insert(0usize) += 1;
        }
        for (index, &(element, marker, marker_range)) in bodies.iter().enumerate() {
            let Some(supported) = references.get(marker) else {
                continue;
            };
            let end = bodies
                .get(index + 1)
                .map_or(rendered_end, |(element, _, _)| element.range.start);
            if marker_range.end >= end
                || existing_notes
                    .iter()
                    .any(|note| note.start <= marker_range.start && marker_range.end <= note.end)
            {
                continue;
            }
            let Some(identity) = footnote_identity(marker, "", None) else {
                continue;
            };
            result.push((
                RawBlock {
                    metadata: MarkupMetadata {
                        start: Some(element.markup_start),
                        attributes: element.attributes.clone(),
                        note_marker: Some(marker_range),
                    },
                    kind: Kind::Footnote,
                    label: identity.label,
                    start: marker_range.end,
                    end,
                    anchor: None,
                    markup_tag: Some(element.tag.clone()),
                    aliases: identity.aliases,
                    parent_label: None,
                },
                if counts.get(marker) == Some(&1) {
                    supported.clone()
                } else {
                    Vec::new()
                },
            ));
        }
    }
    result
}

pub(super) fn supplement_courtlistener_printed_notes(
    structure: &mut DocumentStructure,
    notes: Vec<(RawBlock, Vec<NoteReference>)>,
) {
    for (raw, references) in notes {
        let Some(marker) = raw.metadata.note_marker else {
            continue;
        };
        if structure.notes.iter().any(|note| {
            note.label_range == marker
                || (note.body_range.start <= marker.start && marker.end <= note.body_range.end)
        }) {
            continue;
        }
        // NOTES, a printed label and SUP corroborate a recovered role; the
        // ordinary source P remains Native and is not an explicit footnote tag.
        let mut node = crate::StructureNode::new(
            format!("heuristic-footnote-markup-{}", raw.metadata.start.unwrap()),
            NodeKind::Footnote,
            ScalarRange {
                start: raw.start,
                end: raw.end,
            },
            crate::ENGINE_ORIGIN,
            Derivation::Heuristic,
            None,
        );
        node.label = Some(raw.label);
        node.aliases = Some(raw.aliases);
        node.markup_tag = raw.markup_tag;
        node.markup_start = raw.metadata.start;
        node.markup_attributes = raw.metadata.attributes;
        node.marker_range = Some(marker);
        node.grammar = Some("courtlistener-notes-sup".to_owned());
        structure.notes.push(Note {
            id: format!("note-{}", node.id),
            node_id: Some(node.id.clone()),
            kind: NoteKindV2::Footnote,
            label_range: marker,
            body_range: node.range,
            label_ranges: vec![marker],
            body_ranges: vec![node.range],
            primary_reference: references.first().map(|reference| reference.range),
            references,
        });
        structure.nodes.push(node);
    }
}

/// A printed note's label is not a paragraph number: a recovered numbered paragraph that a
/// note's own marker opens is that note, not the opinion's prose.
pub(super) fn drop_note_numbered_paragraphs(structure: &mut DocumentStructure) {
    let labels = structure.notes.iter()
        .flat_map(|note| note.label_ranges.iter().copied().chain([note.label_range]))
        .filter(|range| range.start < range.end)
        .collect::<Vec<_>>();
    let dropped = structure.nodes.iter()
        .filter(|node| node.kind == NodeKind::Paragraph && node.source == Derivation::Heuristic
            && node.marker_range.is_some_and(|marker|
                labels.iter().any(|label| label.start < marker.end && marker.start < label.end)))
        .map(|node| (node.id.clone(), node.parent_id.clone()))
        .collect::<HashMap<_, _>>();
    if dropped.is_empty() {
        return;
    }
    structure.nodes.retain(|node| !dropped.contains_key(&node.id));
    for node in &mut structure.nodes {
        while let Some(parent) = node.parent_id.as_ref().and_then(|id| dropped.get(id)).cloned() {
            node.parent_id = parent;
        }
    }
    for diagnostic in &mut structure.diagnostics {
        diagnostic.node_ids.retain(|id| !dropped.contains_key(id));
    }
}

pub(super) fn courtlistener_uncontained_note_blocks(
    markup: &str,
    elements: &[MarkupElement],
    text: &str,
    existing: &[RawBlock],
) -> Vec<RawBlock> {
    let candidates = elements
        .iter()
        .enumerate()
        .filter(|(_, element)| {
            element.tag.eq_ignore_ascii_case("a")
                && element
                    .attributes
                    .get("name")
                    .is_some_and(|name| !name.is_empty())
                && ((element
                    .attributes
                    .get("href")
                    .is_some_and(|href| href.starts_with('#'))
                    && markup
                        .get(element.markup_start..element.markup_end)
                        .is_some_and(|raw| raw.contains("Footnote ")))
                    || (element
                        .attributes
                        .get("name")
                        .is_some_and(|name| name.starts_with("footnote"))
                        && !element.attributes.contains_key("href")))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }
    let coordinates = ScalarText::new(text);
    let mut anchors = HashMap::<&str, Option<usize>>::new();
    for (index, element) in elements.iter().enumerate() {
        for name in [
            element.attributes.get("id"),
            element
                .tag
                .eq_ignore_ascii_case("a")
                .then(|| element.attributes.get("name"))
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .filter(|name| !name.is_empty())
        {
            anchors
                .entry(name.as_str())
                .and_modify(|owner| {
                    if *owner != Some(index) {
                        *owner = None;
                    }
                })
                .or_insert(Some(index));
        }
    }
    let gap = |start, end| {
        markup
            .get(start..end)
            .is_some_and(|raw| trim_js(raw).is_empty())
    };
    let inner = |element: &MarkupElement| {
        let raw = markup.get(element.markup_start..element.markup_end)?;
        let start = markup_tag_end(raw)? + 1;
        let end = raw.rfind("</")?;
        let (tag, _) = parse_tag(&raw[end..], true)?;
        (tag.eq_ignore_ascii_case(&element.tag))
            .then(|| raw.get(start..end))
            .flatten()
    };
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label_pattern = LABEL.get_or_init(|| Regex::new(r"^Footnote ([0-9]{1,5}):$").unwrap());
    let mut result = Vec::new();
    let descendant = |index: usize, ancestor: usize| {
        let mut parent = elements[index].parent;
        while let Some(index) = parent {
            if index == ancestor {
                return true;
            }
            parent = elements[index].parent;
        }
        false
    };
    let decoded_gap = |start, end| {
        markup
            .get(start..end)
            .is_some_and(|raw| trim_js(&decode_entities(raw)).is_empty())
    };
    let mut paragraph_notes = Vec::new();
    for &(note_index, note) in &candidates {
        let Some(name) = note.attributes.get("name") else {
            continue;
        };
        let Some(number) = name.strip_prefix("footnote").filter(|number| {
            !number.is_empty()
                && number.len() <= 5
                && number.bytes().all(|byte| byte.is_ascii_digit())
        }) else {
            continue;
        };
        if note.attributes.contains_key("href")
            || anchors.get(name.as_str()).copied().flatten() != Some(note_index)
        {
            continue;
        }
        let Some(paragraph_index) = note.parent else {
            continue;
        };
        let paragraph = &elements[paragraph_index];
        if !paragraph.tag.eq_ignore_ascii_case("p")
            || inner(paragraph).is_none()
            || elements.iter().enumerate().any(|(index, element)| {
                element.tag.eq_ignore_ascii_case("p") && descendant(index, paragraph_index)
            })
            || existing.iter().any(|block| {
                block.metadata.start == Some(note.markup_start)
                    || block.kind == Kind::Footnote
                        && block.start < paragraph.range.end
                        && paragraph.range.start < block.end
            })
        {
            continue;
        }
        let Some((_, marker)) = elements
            .iter()
            .enumerate()
            .find(|(_, element)| element.parent == Some(note_index))
        else {
            continue;
        };
        if !marker.tag.eq_ignore_ascii_case("span")
            || marker.range.start != paragraph.range.start
            || note.range.start != marker.range.start
            || coordinates
                .slice_utf16(marker.range.start..marker.range.end)
                .map(trim_js)
                != Some(number)
            || !coordinates
                .slice_utf16(marker.range.end..paragraph.range.end)
                .is_some_and(|body| body.chars().any(char::is_alphabetic))
        {
            continue;
        }
        let mut incoming = elements.iter().filter(|element| {
            element.tag.eq_ignore_ascii_case("a")
                && element
                    .attributes
                    .get("href")
                    .and_then(|href| href.rsplit_once('#'))
                    .is_some_and(|(_, target)| target == name.as_str())
        });
        let Some(reference) = incoming.next() else {
            continue;
        };
        if incoming.next().is_some()
            || reference.markup_end >= paragraph.markup_start
            || !reference
                .attributes
                .get("href")
                .is_some_and(|href| href.starts_with("file:"))
            || coordinates
                .slice_utf16(reference.range.start..reference.range.end)
                .map(trim_js)
                != Some(number)
        {
            continue;
        }
        // A bookmark can close mid-note. Admit the rest only where explicit
        // same-name bookmark spans cover it inside this complete paragraph.
        let mut covered = note.range.end;
        for (_, element) in elements.iter().enumerate().filter(|(index, element)| {
            element.tag.eq_ignore_ascii_case("span")
                && element.markup_start >= note.markup_end
                && descendant(*index, paragraph_index)
                && element.attributes.get("style").is_some_and(|style| {
                    style.split(';').any(|declaration| {
                        declaration
                            .split_once(':')
                            .is_some_and(|(property, value)| {
                                property.trim().eq_ignore_ascii_case("mso-bookmark")
                                    && value.trim() == name.as_str()
                            })
                    })
                })
        }) {
            if element.range.start > covered
                && !coordinates
                    .slice_utf16(covered..element.range.start)
                    .is_some_and(|gap| trim_js(gap).is_empty())
            {
                break;
            }
            covered = covered.max(element.range.end);
        }
        if !coordinates
            .slice_utf16(covered..paragraph.range.end)
            .is_some_and(|tail| trim_js(tail).is_empty())
        {
            continue;
        }
        paragraph_notes.push((note, paragraph, number, marker.range));
    }
    if let (Some(first), Some(last)) = (paragraph_notes.first(), paragraph_notes.last()) {
        let first_paragraph = first.1;
        let last_paragraph = last.1;
        let mut ancestor_tags = Vec::new();
        let mut parent = last_paragraph.parent;
        while let Some(index) = parent {
            ancestor_tags.push(elements[index].tag.as_str());
            parent = elements[index].parent;
        }
        let literal_empty_tail = markup
            .get(last_paragraph.markup_end..)
            .is_some_and(|mut tail| {
                while let Some(at) = tail.find('<') {
                    if !trim_js(&decode_entities(&tail[..at])).is_empty() {
                        return false;
                    }
                    let raw = &tail[at..];
                    let Some(end) = markup_tag_end(raw) else {
                        return false;
                    };
                    let closing = raw.starts_with("</");
                    let Some((tag, _)) = parse_tag(&raw[..end + 1], closing) else {
                        return false;
                    };
                    if !tag.eq_ignore_ascii_case("p")
                        && !tag.eq_ignore_ascii_case("span")
                        && !(closing
                            && ancestor_tags
                                .iter()
                                .any(|ancestor| tag.eq_ignore_ascii_case(ancestor)))
                    {
                        return false;
                    }
                    tail = &raw[end + 1..];
                }
                trim_js(&decode_entities(tail)).is_empty()
            });
        let owners = paragraph_notes
            .iter()
            .map(|(_, paragraph, _, _)| paragraph.markup_start)
            .collect::<HashSet<_>>();
        let siblings = elements
            .iter()
            .filter(|element| {
                element.parent == first_paragraph.parent
                    && element.markup_start >= first_paragraph.markup_start
            })
            .collect::<Vec<_>>();
        let coherent = paragraph_notes
            .iter()
            .all(|(_, paragraph, _, _)| paragraph.parent == first_paragraph.parent)
            && siblings.iter().all(|element| {
                element.tag.eq_ignore_ascii_case("p")
                    && (owners.contains(&element.markup_start)
                        || coordinates
                            .slice_utf16(element.range.start..element.range.end)
                            .is_some_and(|text| trim_js(text).is_empty()))
            })
            && siblings
                .windows(2)
                .all(|pair| decoded_gap(pair[0].markup_end, pair[1].markup_start))
            && coordinates
                .slice_utf16(last_paragraph.range.end..coordinates.utf16_len())
                .is_some_and(|tail| trim_js(tail).is_empty());
        let separated = elements
            .iter()
            .rev()
            .find(|element| {
                element.tag.eq_ignore_ascii_case("hr")
                    && element.markup_end <= first_paragraph.markup_start
            })
            .is_some_and(|separator| {
                let mut parent = separator.parent;
                let mut end = separator.markup_end;
                while parent != first_paragraph.parent {
                    let Some(index) = parent else { return false };
                    let owner = &elements[index];
                    let Some(raw) = markup.get(end..owner.markup_end) else {
                        return false;
                    };
                    let decoded = decode_entities(raw);
                    let raw = trim_js(&decoded);
                    if !raw.starts_with("</")
                        || markup_tag_end(raw).map(|at| at + 1) != Some(raw.len())
                        || !parse_tag(raw, true).is_some_and(|(tag, attributes)| {
                            tag.eq_ignore_ascii_case(&owner.tag) && attributes.is_empty()
                        })
                    {
                        return false;
                    }
                    end = owner.markup_end;
                    parent = owner.parent;
                }
                decoded_gap(end, first_paragraph.markup_start)
            });
        if coherent && separated && literal_empty_tail {
            for (note, paragraph, number, marker) in paragraph_notes {
                let Some(identity) =
                    footnote_identity(number, "", note.attributes.get("name").cloned())
                else {
                    continue;
                };
                result.push(RawBlock {
                    metadata: MarkupMetadata {
                        start: Some(note.markup_start),
                        attributes: note.attributes.clone(),
                        note_marker: Some(marker),
                    },
                    kind: Kind::Footnote,
                    label: identity.label,
                    start: marker.end,
                    end: paragraph.range.end,
                    anchor: identity.anchor,
                    markup_tag: Some(note.tag.clone()),
                    aliases: identity.aliases,
                    parent_label: None,
                });
            }
        }
    }
    for (note_index, note) in candidates {
        let Some(raw_label) = coordinates.slice_utf16(note.range.start..note.range.end) else {
            continue;
        };
        let label = trim_js(raw_label);
        let Some(capture) = label_pattern.captures(label) else {
            continue;
        };
        let Some(name) = note.attributes.get("name") else {
            continue;
        };
        if anchors.get(name.as_str()).copied().flatten() != Some(note_index)
            || existing.iter().any(|block| {
                block.metadata.start == Some(note.markup_start)
                    || block.kind == Kind::Footnote
                        && block.start <= note.range.start
                        && note.range.end <= block.end
            })
        {
            continue;
        }
        let Some(reference_name) = note
            .attributes
            .get("href")
            .and_then(|href| href.strip_prefix('#'))
        else {
            continue;
        };
        let Some(reference_index) = anchors.get(reference_name).copied().flatten() else {
            continue;
        };
        let reference = &elements[reference_index];
        if !reference.tag.eq_ignore_ascii_case("a")
            || reference.markup_end >= note.markup_start
            || reference.attributes.get("name").map(String::as_str) != Some(reference_name)
            || reference
                .attributes
                .get("href")
                .and_then(|href| href.strip_prefix('#'))
                != Some(name.as_str())
            || coordinates
                .slice_utf16(reference.range.start..reference.range.end)
                .map(trim_js)
                != Some(format!("[FN{}]", &capture[1]).as_str())
        {
            continue;
        }
        let Some(parent) = note.parent else {
            continue;
        };
        let siblings = elements
            .iter()
            .filter(|element| element.parent == Some(parent))
            .collect::<Vec<_>>();
        let Some(position) = siblings
            .iter()
            .position(|element| element.markup_start == note.markup_start)
        else {
            continue;
        };
        if position < 3 {
            continue;
        }
        let preceding = &siblings[position - 3..position];
        let heading = preceding[0];
        if !heading.tag.eq_ignore_ascii_case("div")
            || reference.markup_end >= heading.markup_start
            || coordinates
                .slice_utf16(heading.range.start..heading.range.end)
                .map(trim_js)
                != Some("Footnotes")
            || preceding[1..]
                .iter()
                .any(|element| !element.tag.eq_ignore_ascii_case("br"))
            || !gap(heading.markup_end, preceding[1].markup_start)
            || !gap(preceding[1].markup_end, preceding[2].markup_start)
            || !gap(preceding[2].markup_end, note.markup_start)
        {
            continue;
        }
        let following = &siblings[position + 1..];
        let Some(separator_at) = following
            .iter()
            .position(|element| element.tag.eq_ignore_ascii_case("br"))
        else {
            continue;
        };
        // ponytail: only the witnessed terminal single fragment; broader inline
        // layouts and continuations need their own independent source evidence.
        if following[..separator_at]
            .iter()
            .any(|element| !element.tag.eq_ignore_ascii_case("i"))
            || following.len() != separator_at + 5
        {
            continue;
        }
        let separators = &following[separator_at..separator_at + 4];
        let navigation = following[separator_at + 4];
        if separators
            .iter()
            .any(|element| !element.tag.eq_ignore_ascii_case("br"))
            || separators
                .windows(2)
                .any(|pair| !gap(pair[0].markup_end, pair[1].markup_start))
            || !gap(separators[3].markup_end, navigation.markup_start)
            || !navigation.tag.eq_ignore_ascii_case("div")
        {
            continue;
        }
        let Some(navigation_index) = elements
            .binary_search_by_key(&navigation.markup_start, |element| element.markup_start)
            .ok()
        else {
            continue;
        };
        let mut children = elements
            .iter()
            .enumerate()
            .filter(|(_, element)| element.parent == Some(navigation_index));
        let Some((form_index, form)) = children.next() else {
            continue;
        };
        if children.next().is_some()
            || !form.tag.eq_ignore_ascii_case("form")
            || !form
                .attributes
                .get("method")
                .is_some_and(|method| method.eq_ignore_ascii_case("LINK"))
            || !form
                .attributes
                .get("action")
                .is_some_and(|action| !action.is_empty())
            || elements
                .iter()
                .any(|element| element.parent == Some(form_index))
            || !inner(form).is_some_and(|raw| trim_js(raw).is_empty())
            || inner(navigation).map(trim_js)
                != markup.get(form.markup_start..form.markup_end).map(trim_js)
        {
            continue;
        }
        let end = separators[0].range.start;
        if !coordinates
            .slice_utf16(end..coordinates.utf16_len())
            .is_some_and(|tail| trim_js(tail).is_empty())
        {
            continue;
        }
        let Some(raw_body) = coordinates.slice_utf16(note.range.end..end) else {
            continue;
        };
        let body = trim_js(raw_body);
        if body.is_empty() || !body.chars().any(char::is_alphabetic) {
            continue;
        }
        let body_leading =
            raw_body.len() - raw_body.trim_start_matches(javascript_whitespace).len();
        let start = note.range.end + utf16_len(&raw_body[..body_leading]);
        let label_leading =
            raw_label.len() - raw_label.trim_start_matches(javascript_whitespace).len();
        let marker_start = note.range.start + utf16_len(&raw_label[..label_leading]);
        let Some(mut identity) = footnote_identity(&capture[1], "", Some(name.clone())) else {
            continue;
        };
        identity.aliases.push(label.to_owned());
        result.push(RawBlock {
            metadata: MarkupMetadata {
                start: Some(note.markup_start),
                attributes: note.attributes.clone(),
                note_marker: Some(ScalarRange {
                    start: marker_start,
                    end: marker_start + utf16_len(label),
                }),
            },
            kind: Kind::Footnote,
            label: identity.label,
            start,
            end: start + utf16_len(body),
            anchor: identity.anchor,
            markup_tag: Some(note.tag.clone()),
            aliases: identity.aliases,
            parent_label: None,
        });
    }
    result
}

pub(super) fn native_table_coordinates(structure: &mut DocumentStructure) {
    let mut tables = structure
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Table && node.markup_start.is_some())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    tables.sort_by_key(|&index| structure.nodes[index].markup_start);
    let coordinates = ScalarText::new(structure.query_text());
    let mut replacements = HashMap::<String, String>::new();
    let mut grids = HashMap::<String, (Option<usize>, Option<usize>, Option<usize>)>::new();
    let mut table_headers = HashMap::<String, Vec<usize>>::new();
    for (table_index, &table) in tables.iter().enumerate() {
        let table_id = format!("table:{}", table_index + 1);
        let mut rows = structure
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.kind == NodeKind::Row
                    && node.parent_id.as_deref() == Some(&structure.nodes[table].id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        rows.sort_by_key(|&index| structure.nodes[index].markup_start);
        let mut facts = Vec::new();
        let mut grid = vec![(structure.nodes[table].id.clone(), (Some(table_index + 1), None, None))];
        let mut ids = vec![(structure.nodes[table].id.clone(), table_id.clone())];
        let mut occupied = Vec::<(usize, usize, usize)>::new();
        let mut header_rows = Vec::new();
        let mut valid = true;
        'rows: for (row_index, &row) in rows.iter().enumerate() {
            let row_number = row_index + 1;
            let row_id = format!("{table_id}/row:{row_number}");
            ids.push((structure.nodes[row].id.clone(), row_id.clone()));
            grid.push((structure.nodes[row].id.clone(), (None, Some(row_number), None)));
            let mut cells = structure
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    node.kind == NodeKind::Cell
                        && node.parent_id.as_deref() == Some(&structure.nodes[row].id)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            cells.sort_by_key(|&index| structure.nodes[index].markup_start);
            if !cells.is_empty() && cells.iter().all(|&index| structure.nodes[index].markup_tag.as_deref() == Some("th")) {
                header_rows.push(row_number);
            }
            let mut column = 1usize;
            for &cell in &cells {
                let node = &structure.nodes[cell];
                while let Some(end) = occupied
                    .iter()
                    .filter(|&&(start, end, last_row)| {
                        start <= column && column <= end && row_number <= last_row
                    })
                    .map(|&(_, end, _)| end)
                    .max()
                {
                    let Some(next) = end.checked_add(1) else {
                        valid = false;
                        break 'rows;
                    };
                    column = next;
                }
                let span = node.column_span.unwrap_or(1);
                let Some(last_column) = span
                    .checked_sub(1)
                    .and_then(|span| column.checked_add(span))
                else {
                    valid = false;
                    break 'rows;
                };
                let Some(last_row) = node
                    .row_span
                    .unwrap_or(1)
                    .checked_sub(1)
                    .and_then(|span| row_number.checked_add(span))
                else {
                    valid = false;
                    break 'rows;
                };
                occupied.push((column, last_column, last_row));
                let range = structure.query_range(node);
                facts.push(crate::AuthoritativeTableCell {
                    ranges: structure.query_ranges(node).into_owned(),
                    table: table_index + 1,
                    table_name: None,
                    row: row_number,
                    column,
                    row_span: node.row_span,
                    column_span: node.column_span,
                    address: node.anchor.clone(),
                    display_value: None,
                    start: range.start,
                    end: range.end,
                });
                ids.push((node.id.clone(), format!("{row_id}/col:{column}")));
                grid.push((node.id.clone(), (None, Some(row_number), Some(column))));
                let Some(next) = last_column.checked_add(1) else {
                    valid = false;
                    break 'rows;
                };
                column = next;
            }
        }
        // The existing table owner validates address/span and text coordinates.
        // Unusable geometry keeps its complete source graph and attributes.
        if valid && crate::tables::AuthoritativeTables::new(&coordinates, &facts).is_ok() {
            table_headers.insert(structure.nodes[table].id.clone(), header_rows);
            replacements.extend(ids);
            grids.extend(grid);
        }
    }
    for node in &mut structure.nodes {
        if let Some(headers) = table_headers.remove(&node.id) { node.header_rows = Some(headers); }
        if let Some(&(table, row, column)) = grids.get(&node.id) {
            node.table_number = table;
            node.row = row;
            node.column = column;
        }
        if let Some(id) = replacements.get(&node.id) {
            node.id = id.clone();
            node.label = Some(id.clone());
        }
        if let Some(parent) = node.parent_id.as_ref() {
            if let Some(id) = replacements.get(parent) {
                node.parent_id = Some(id.clone());
            }
        }
    }
}

pub(super) fn prefer_source_headings(structure: &mut DocumentStructure) {
    let mut headings = HashMap::new();
    for node in &structure.nodes {
        if node.kind == NodeKind::Heading
            && node.source == Derivation::Native
            && node.markup_start.is_some()
            && node.markup_tag.is_some()
        {
            let range = structure.query_range(node);
            headings
                .entry((range.start, range.end))
                .and_modify(|id| *id = None)
                .or_insert_with(|| Some(node.id.clone()));
        }
    }
    let replacements = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Heading && node.source == Derivation::Heuristic)
        .filter_map(|node| {
            let range = structure.query_range(node);
            let native = headings.get(&(range.start, range.end))?.as_ref()?;
            Some((node.id.clone(), native.clone()))
        })
        .collect::<HashMap<_, _>>();
    // Resolve exact duplicate inference after IDs are assigned so unrelated
    // inferred nodes keep their identities. Partial overlaps remain distinct.
    structure
        .nodes
        .retain(|node| !replacements.contains_key(&node.id));
    for node in &mut structure.nodes {
        if let Some(native) = node.parent_id.as_ref().and_then(|id| replacements.get(id)) {
            node.parent_id = Some(native.clone());
        }
    }
    for diagnostic in &mut structure.diagnostics {
        for id in &mut diagnostic.node_ids {
            if let Some(native) = replacements.get(id) {
                *id = native.clone();
            }
        }
    }
}

pub(super) fn prefer_source_note_markers(structure: &mut DocumentStructure) {
    let mut markers = HashMap::new();
    for node in &structure.nodes {
        if node.kind == NodeKind::Footnote
            && node.source == Derivation::Native
            && node.markup_start.is_some()
        {
            if let Some(marker) = node.marker_range.filter(|range| range.start < range.end) {
                markers
                    .entry(marker.start)
                    .and_modify(|id| *id = None)
                    .or_insert_with(|| Some(node.id.clone()));
            }
        }
    }
    let replacements = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Paragraph && node.source == Derivation::Heuristic)
        .filter_map(|node| {
            let native = markers.get(&structure.query_range(node).start)?.as_ref()?;
            Some((node.id.clone(), native.clone()))
        })
        .collect::<HashMap<_, _>>();
    // A declared printed note label is not a guessed paragraph number. Resolve
    // the conflict after ID assignment without clipping earlier paragraphs.
    structure
        .nodes
        .retain(|node| !replacements.contains_key(&node.id));
    for node in &mut structure.nodes {
        if let Some(native) = node.parent_id.as_ref().and_then(|id| replacements.get(id)) {
            node.parent_id = Some(native.clone());
        }
    }
    for diagnostic in &mut structure.diagnostics {
        for id in &mut diagnostic.node_ids {
            if let Some(native) = replacements.get(id) {
                *id = native.clone();
            }
        }
    }
}

pub(super) fn clip_courtlistener_paragraphs_to_opinions(
    structure: &mut DocumentStructure,
    matched_role_starts: &HashSet<usize>,
) {
    let mut native_scopes = HashMap::new();
    for node in &structure.nodes {
        if node.kind == NodeKind::Section && node.source == Derivation::Native {
            if let Some(start) = node
                .markup_start
                .filter(|start| matched_role_starts.contains(start))
            {
                native_scopes
                    .entry(start)
                    .and_modify(|scope| *scope = None)
                    .or_insert(Some(structure.query_range(node)));
            }
        }
    }
    if native_scopes.is_empty() {
        return;
    }
    let mut scopes = Vec::<Option<ScalarRange>>::with_capacity(structure.markup_elements.len());
    let mut paragraph_scopes = HashMap::new();
    let mut writing_starts = Vec::new();
    for element in &structure.markup_elements {
        let declared_opinion = [
            "opinion",
            "opinion_text",
            "concurrence_text",
            "dissent_text",
        ]
        .iter()
        .any(|tag| element.tag.eq_ignore_ascii_case(tag))
            || (element.tag.eq_ignore_ascii_case("div")
                && element.attributes.get("class").is_some_and(|classes| {
                    classes.split_ascii_whitespace().any(|class| {
                        matches!(class, "ldml-opinion" | "de-writing" | "co_opinionBlock")
                    })
                }))
            || (["article", "section", "div"]
                .iter()
                .any(|tag| element.tag.eq_ignore_ascii_case(tag))
                && element.attributes.get("class").is_some_and(|classes| {
                    classes
                        .split_ascii_whitespace()
                        .any(|class| class == "opinion")
                }));
        let scope = if declared_opinion {
            // An unmatched nearer writing cannot inherit a complete outer scope.
            native_scopes.get(&element.markup_start).copied().flatten()
        } else {
            element.parent.and_then(|parent| scopes[parent])
        };
        scopes.push(scope);
        if declared_opinion {
            if let Some(range) = scope.filter(|range| range.start < range.end) {
                writing_starts.push((element.markup_start, range.start));
            }
        }
        if element.tag.eq_ignore_ascii_case("p") && element.range.start < element.range.end {
            paragraph_scopes
                .entry(element.range.start)
                .and_modify(|scope| *scope = None)
                .or_insert(scope.map(|scope| (scope, element.range.end, element.markup_end)));
        }
    }
    for node in &mut structure.nodes {
        if node.kind == NodeKind::Paragraph && node.source == Derivation::Heuristic {
            if let Some((scope, paragraph_end, markup_end)) =
                paragraph_scopes.get(&node.range.start).copied().flatten()
            {
                node.range.end = node.range.end.min(scope.end);
                // A separately supplied writing after the literal P is also a
                // boundary, including a writing nested inside the same opinion.
                // Keep embedded writing markup inside the P and do not equate
                // the physical P end with the legal paragraph's end.
                let next = writing_starts.partition_point(|&(start, _)| start < markup_end);
                if let Some((_, start)) = writing_starts[next..]
                    .iter()
                    .find(|&&(_, start)| start >= paragraph_end && start > node.range.start)
                {
                    node.range.end = node.range.end.min(*start);
                }
            }
        }
    }
}

/// Pages a CourtListener opinion prints only as text: a New York slip opinion's "[*2]" and a
/// Connecticut Superior Court opinion's "CT Page 6807", each opening the page it numbers.
pub(super) fn courtlistener_printed_star_pages(structure: &mut DocumentStructure) {
    if structure.nodes.iter().any(|node| node.kind == NodeKind::Page) {
        return;
    }
    static MARKER: OnceLock<Regex> = OnceLock::new();
    let text = structure.query_text();
    let mut markers = Vec::<(usize, usize, u32)>::new();
    let (mut byte, mut unit) = (0, 0);
    for capture in MARKER
        .get_or_init(|| Regex::new(r"\[\*(\d{1,5})\]|\bCT Page (\d{1,5})\b").unwrap())
        .captures_iter(text)
    {
        let whole = capture.get(0).unwrap();
        unit += utf16_len(&text[byte..whole.start()]);
        byte = whole.start();
        let Ok(number) = capture.get(1).or_else(|| capture.get(2)).unwrap().as_str().parse::<u32>() else {
            continue;
        };
        // One numbering, read forward: each page follows the one before it.
        if markers.last().is_some_and(|&(_, _, last)| number != last + 1) {
            continue;
        }
        markers.push((unit, unit + utf16_len(whole.as_str()), number));
    }
    let end = utf16_len(text);
    let pages = markers.iter().enumerate().map(|(index, &(start, marker_end, number))| {
        let mut node = crate::StructureNode::new(
            format!("heuristic-printed-page-{}", index + 1),
            NodeKind::Page,
            ScalarRange { start, end: markers.get(index + 1).map_or(end, |next| next.0) },
            crate::ENGINE_ORIGIN,
            Derivation::Heuristic,
            None,
        );
        node.label = Some(format!("page{number}"));
        node.aliases = Some(vec![format!("*{number}")]);
        node.marker_range = Some(ScalarRange { start, end: marker_end });
        node.grammar = Some("courtlistener-printed-star-page".to_owned());
        node
    }).collect::<Vec<_>>();
    structure.nodes.extend(pages);
}

/// A New York slip opinion's notes, printed as text: "[FN1]" in the opinion and, under a "Footnotes" line,
/// "Footnote 1: ..." for each. Each note runs to the next; every one must be referenced and they must run 1, 2, 3...
pub(super) fn courtlistener_slip_notes(structure: &mut DocumentStructure) {
    if structure.nodes.iter().any(|node| node.kind == NodeKind::Footnote) {
        return;
    }
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static BODY: OnceLock<Regex> = OnceLock::new();
    static REFERENCE: OnceLock<Regex> = OnceLock::new();
    let text = structure.query_text();
    let Some(head) = HEAD.get_or_init(|| Regex::new(r"(?m)^[ \t]*Footnotes[ \t]*$").unwrap()).find(text) else {
        return;
    };
    let bodies = BODY.get_or_init(|| Regex::new(r"(?m)^[ \t]*(Footnote ([0-9]{1,3}):)").unwrap())
        .captures_iter(&text[head.end()..])
        .filter_map(|capture| {
            let label = capture.get(1)?;
            Some((capture[2].parse::<usize>().ok()?, head.end() + label.start(), head.end() + label.end()))
        })
        .collect::<Vec<_>>();
    if bodies.is_empty() || bodies.iter().enumerate().any(|(index, body)| body.0 != index + 1) {
        return;
    }
    let mut references = HashMap::<usize, Vec<ScalarRange>>::new();
    for capture in REFERENCE.get_or_init(|| Regex::new(r"\[FN([0-9]{1,3})\]").unwrap()).captures_iter(&text[..head.start()]) {
        let (Some(whole), Ok(number)) = (capture.get(0), capture[1].parse::<usize>()) else { continue };
        let start = utf16_len(&text[..whole.start()]);
        references.entry(number).or_default().push(ScalarRange { start, end: start + utf16_len(whole.as_str()) });
    }
    if bodies.iter().any(|body| !references.contains_key(&body.0)) {
        return;
    }
    let mut additions = Vec::new();
    for (index, &(number, label_start, label_end)) in bodies.iter().enumerate() {
        let end = bodies.get(index + 1).map_or(text.len(), |next| next.1);
        let body_end = label_start + text[label_start..end].trim_end_matches(javascript_whitespace).len();
        let label = ScalarRange { start: utf16_len(&text[..label_start]), end: utf16_len(&text[..label_end]) };
        let range = ScalarRange { start: label.start, end: utf16_len(&text[..body_end]) };
        let refs = references.remove(&number).unwrap_or_default();
        let mut node = crate::StructureNode::new(
            format!("heuristic-slip-note-{number}"),
            NodeKind::Footnote,
            range,
            crate::ENGINE_ORIGIN,
            Derivation::Heuristic,
            None,
        );
        node.label = Some(format!("fn{number}"));
        node.marker_range = Some(label);
        node.grammar = Some("courtlistener-slip-notes".to_owned());
        let note = Note {
            id: format!("note-{}", node.id),
            node_id: Some(node.id.clone()),
            kind: NoteKindV2::Footnote,
            label_range: label,
            body_range: range,
            label_ranges: vec![label],
            body_ranges: vec![range],
            primary_reference: refs.first().copied(),
            references: refs.into_iter().map(|range| NoteReference { range, anchor: None, page_indexes: Vec::new(), line_ids: Vec::new() }).collect(),
        };
        additions.push((node, note));
    }
    for (node, note) in additions {
        structure.nodes.push(node);
        structure.notes.push(note);
    }
}

/// An opinion's opening page: the text before the first page marker of a numbering sits on the
/// page before it, so "*289" opens page 289 and what precedes it in the opinion is page 288.
pub(super) fn courtlistener_opening_pages(structure: &mut DocumentStructure) {
    const TRACK: [&str; 3] = ["citation-index", "data-citation-index", "pagescheme"];
    let pages = structure.nodes.iter().enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Page)
        .map(|(index, node)| (index, structure.query_range(node)))
        .collect::<Vec<_>>();
    let note_ids = structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote)
        .map(|node| node.id.clone()).collect::<HashSet<_>>();
    let track_of = |node: &crate::StructureNode| TRACK.iter()
        .filter_map(|name| node.markup_attributes.get(*name).cloned()).collect::<Vec<_>>();
    // The first page of each numbering in each opinion; a page a note owns is that note's.
    let mut firsts = BTreeMap::<(Vec<String>, Option<String>), (usize, ScalarRange)>::new();
    for &(index, range) in &pages {
        let node = &structure.nodes[index];
        if node.parent_id.as_ref().is_some_and(|parent| note_ids.contains(parent)) {
            continue;
        }
        firsts.entry((track_of(node), node.parent_id.clone()))
            .and_modify(|first| if range.start < first.1.start { *first = (index, range) })
            .or_insert((index, range));
    }
    let coordinates = ScalarText::new(structure.query_text());
    let mut opening = Vec::new();
    for ((track, parent), (index, range)) in firsts {
        let first = &structure.nodes[index];
        let Some(printed) = first.label.as_deref().and_then(|label| label.strip_prefix("page")) else {
            continue;
        };
        let Some(before) = previous_page(printed) else {
            continue;
        };
        let scope_start = parent.as_ref()
            .and_then(|id| structure.nodes.iter().find(|node| &node.id == id))
            .map_or(0, |node| structure.query_range(node).start);
        let end = range.start;
        let Some(gap) = coordinates.slice_utf16(scope_start..end) else {
            continue;
        };
        if gap.trim_matches(javascript_whitespace).is_empty() {
            continue;
        }
        let start = scope_start + utf16_len(&gap[..gap.len() - gap.trim_start_matches(javascript_whitespace).len()]);
        // Another numbering's page may cover the same text; this numbering's may not.
        if pages.iter().any(|(other, span)| span.start < end && start < span.end
            && track_of(&structure.nodes[*other]) == track) {
            continue;
        }
        let mut node = crate::StructureNode::new(
            format!("heuristic-opening-page-{}", opening.len() + 1),
            NodeKind::Page,
            ScalarRange { start, end },
            crate::ENGINE_ORIGIN,
            Derivation::Heuristic,
            parent,
        );
        node.label = Some(format!("page{before}"));
        node.aliases = first.aliases.as_ref().map(|aliases| aliases.iter()
            .filter_map(|alias| alias.strip_suffix(printed).map(|stem| format!("{stem}{before}")))
            .collect());
        node.markup_attributes = TRACK.iter()
            .filter_map(|name| Some((name.to_string(), first.markup_attributes.get(*name)?.clone())))
            .collect();
        node.grammar = Some("courtlistener-opening-page".to_owned());
        opening.push(node);
    }
    structure.nodes.extend(opening);
}

/// The page printed before a page: 289 before 290, and an inserted page's own run, 595 before 595a and
/// 595a before 595b.
pub(super) fn previous_page(printed: &str) -> Option<String> {
    if !printed.is_empty() && printed.len() <= 5 && printed.bytes().all(|byte| byte.is_ascii_digit()) {
        return printed.parse::<u32>().ok().filter(|number| *number >= 2).map(|number| (number - 1).to_string());
    }
    let letter = printed.chars().next_back().filter(char::is_ascii_lowercase)?;
    let digits = &printed[..printed.len() - 1];
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(if letter == 'a' { digits.to_owned() } else { format!("{digits}{}", (letter as u8 - 1) as char) })
}

pub(super) fn class_listed(attributes: &BTreeMap<String, String>, wanted: &str) -> bool {
    attributes.get("class").is_some_and(|classes| classes.split_ascii_whitespace().any(|class| class == wanted))
}

/// A public.resource.org paragraph prints its own number in a SPAN.num before its text.
pub(super) fn courtlistener_resource_paragraph_markers(structure: &mut DocumentStructure) {
    let coordinates = ScalarText::new(structure.query_text());
    let elements = &structure.markup_elements;
    let markers = structure.nodes.iter().enumerate().filter_map(|(index, node)| {
        if node.kind != NodeKind::Paragraph || node.marker_range.is_some()
            || node.markup_tag.as_deref() != Some("div") || !class_listed(&node.markup_attributes, "num")
        {
            return None;
        }
        let number = node.label.as_deref()?.strip_prefix("par")?;
        let element = elements.binary_search_by_key(&node.markup_start?, |element| element.markup_start).ok()?;
        let span = elements[element + 1..].iter()
            .take_while(|child| child.markup_start < elements[element].markup_end)
            .find(|child| child.parent == Some(element) && child.tag == "span" && class_listed(&child.attributes, "num"))?;
        let raw = coordinates.slice_utf16(span.range.start..span.range.end)?;
        let printed = trim_js(raw);
        if printed != number {
            return None;
        }
        let start = span.range.start + utf16_len(&raw[..raw.len() - raw.trim_start_matches(javascript_whitespace).len()]);
        Some((index, ScalarRange { start, end: start + utf16_len(printed) }))
    }).collect::<Vec<_>>();
    for (index, marker) in markers {
        structure.nodes[index].marker_range = Some(marker);
    }
}

/// CAP's BRACKETNUM opening a paragraph is the opinion's own division number only where every
/// one is a single numeral and they run 1, 2, 3...; any other BRACKETNUM (a headnote key such as
/// "2. 3." or a marginal catchline) leaves the opinion's paragraphs unnumbered by it.
pub(super) fn courtlistener_bracketnum_divisions(structure: &mut DocumentStructure) {
    if structure.nodes.iter().any(|node| node.kind == NodeKind::Paragraph && node.marker_range.is_some()) {
        return;
    }
    let coordinates = ScalarText::new(structure.query_text());
    let elements = &structure.markup_elements;
    let mut divisions = Vec::new();
    for element in elements {
        if element.tag != "bracketnum" {
            continue;
        }
        let Some(parent) = element.parent.filter(|parent| elements[*parent].tag == "p") else {
            continue;
        };
        let Some(before) = coordinates.slice_utf16(elements[parent].range.start..element.range.start) else {
            return;
        };
        if !before.trim_matches(javascript_whitespace).is_empty() {
            continue;
        }
        let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end) else {
            return;
        };
        let printed = trim_js(raw);
        let Some(number) = printed.strip_suffix('.').filter(|number| (1..=3).contains(&number.len())
            && number.bytes().all(|byte| byte.is_ascii_digit())) else {
            return;
        };
        let Some(node) = structure.nodes.iter().position(|node| node.source == Derivation::Native
            && node.markup_start == Some(elements[parent].markup_start)
            && matches!(node.kind, NodeKind::Prose | NodeKind::Paragraph)) else {
            return;
        };
        let start = element.range.start + utf16_len(&raw[..raw.len() - raw.trim_start_matches(javascript_whitespace).len()]);
        divisions.push((node, number.parse::<usize>().unwrap(), ScalarRange { start, end: start + utf16_len(printed) }));
    }
    if divisions.len() < 2 || divisions.iter().enumerate().any(|(index, division)| division.1 != index + 1) {
        return;
    }
    for (index, number, marker) in divisions {
        let node = &mut structure.nodes[index];
        node.kind = NodeKind::Paragraph;
        node.label = Some(format!("par{number}"));
        node.marker_range = Some(marker);
    }
}

/// An opinion's divisions printed as paragraphs of their own, "I", "II", "A.": headings, where
/// the opinion opens its divisions with "I".
pub(super) fn courtlistener_division_headings(structure: &mut DocumentStructure) {
    static DIVISION: OnceLock<Regex> = OnceLock::new();
    let division = DIVISION.get_or_init(|| Regex::new(r"^(?:[IVX]{1,5}|[A-H])\.?$").unwrap());
    let coordinates = ScalarText::new(structure.query_text());
    // A heading the engine already reads there stays the one heading.
    let existing = structure.nodes.iter().filter(|node| node.kind == NodeKind::Heading)
        .map(|node| structure.query_range(node)).collect::<Vec<_>>();
    let headings = structure.nodes.iter().enumerate().filter_map(|(index, node)| {
        if node.source != Derivation::Native || !matches!(node.kind, NodeKind::Prose | NodeKind::Paragraph)
            || node.marker_range.is_some() || node.markup_tag.as_deref() != Some("p")
        {
            return None;
        }
        let range = structure.query_range(node);
        let printed = trim_js(coordinates.slice_utf16(range.start..range.end)?);
        let read = existing.iter().any(|heading| heading.start < range.end && range.start < heading.end);
        division.is_match(printed).then(|| (index, printed.trim_end_matches('.') == "I", read))
    }).collect::<Vec<_>>();
    if headings.len() < 2 || !headings.iter().any(|(_, first, _)| *first) {
        return;
    }
    for (index, _, _) in headings.into_iter().filter(|(_, _, read)| !read) {
        let range = structure.query_range(&structure.nodes[index]);
        let node = &mut structure.nodes[index];
        node.kind = NodeKind::Heading;
        node.role = Some(crate::PrimaryRole::Heading);
        node.heading_range = Some(range);
    }
}

/// The CAP front-matter field a data-type attribute names ("legal" is the reporter's arguments of counsel).
pub(super) fn cap_front_matter(typed: &str) -> Option<&'static str> {
    Some(match typed {
        "syllabus" => "syllabus", "summary" => "summary", "parties" => "parties", "docketnumber" => "docketnumber",
        "decisiondate" => "decisiondate", "otherdate" => "otherdate", "court" => "court", "judges" => "judges",
        "attorneys" => "attorneys", "history" => "history", "disposition" => "disposition", "seealso" => "seealso",
        "correction" => "correction", "arguments" | "legal" => "arguments", "headnotes" => "headnotes",
        _ => return None,
    })
}

/// The front-matter roles a CourtListener source marks: CAP's own elements, and the classes of
/// Westlaw, OSCN, Lexis and public.resource.org renditions, on the block or an ancestor.
pub(super) fn courtlistener_source_roles(structure: &mut DocumentStructure) {
    fn role(element: &MarkupElement) -> Option<crate::PrimaryRole> {
        use crate::PrimaryRole as Role;
        let typed = element.attributes.get("data-type").and_then(|typed| cap_front_matter(typed));
        let by_tag = match typed.unwrap_or(element.tag.as_str()) {
            "syllabus" | "summary" | "headnotes" | "history" | "seealso" | "correction" | "arguments" => Some(Role::Headnote),
            "parties" | "fullcasename" | "partyblock" => Some(Role::Caption),
            "judges" => Some(Role::Judges),
            "attorneys" => Some(Role::Counsel),
            "decisiondate" | "otherdate" => Some(Role::Date),
            "docketnumber" => Some(Role::DocketNumber),
            "court" | "courtname" => Some(Role::Court),
            _ => None,
        };
        by_tag.or_else(|| element.attributes.get("class")?.split_ascii_whitespace().find_map(|class| match class {
            "co_partyLine" | "de-style" | "parties" => Some(Role::Caption),
            "co_panelBlock" | "panel" | "judges" => Some(Role::Judges),
            "co_attorneyBlock" | "de-attorneys" | "counsel" => Some(Role::Counsel),
            "co_date" | "date" => Some(Role::Date),
            "co_docketBlock" | "docket" | "slip-docket" => Some(Role::DocketNumber),
            "court" => Some(Role::Court),
            "de-summary" | "courtsummary" | "syllabus" => Some(Role::Headnote),
            _ => None,
        }))
    }
    let elements = &structure.markup_elements;
    let roles = structure.nodes.iter().enumerate().filter_map(|(index, node)| {
        if node.role.is_some() || !matches!(node.kind, NodeKind::Prose | NodeKind::Paragraph | NodeKind::Section) {
            return None;
        }
        let mut at = elements.binary_search_by_key(&node.markup_start?, |element| element.markup_start).ok();
        while let Some(element) = at {
            if let Some(role) = role(&elements[element]) {
                return Some((index, role));
            }
            at = elements[element].parent;
        }
        None
    }).collect::<Vec<_>>();
    for (index, role) in roles {
        structure.nodes[index].role = Some(role);
    }
}

pub(super) fn courtlistener_source_parents(structure: &mut DocumentStructure) {
    let mut owners = HashMap::<usize, Option<usize>>::new();
    for (index, node) in structure.nodes.iter().enumerate() {
        if node.source == Derivation::Native && node.kind != NodeKind::Page {
            if let Some(start) = node.markup_start {
                owners
                    .entry(start)
                    .and_modify(|owner| *owner = None)
                    .or_insert(Some(index));
            }
        }
    }
    let mut parents = Vec::new();
    for (index, node) in structure.nodes.iter().enumerate() {
        if node.source != Derivation::Native || node.kind == NodeKind::Page {
            continue;
        }
        let Some(element_index) = node.markup_start.and_then(|start| {
            structure
                .markup_elements
                .binary_search_by_key(&start, |element| element.markup_start)
                .ok()
        }) else {
            continue;
        };
        let range = structure.query_range(node);
        let mut parent = structure.markup_elements[element_index].parent;
        while let Some(element_index) = parent {
            let element = &structure.markup_elements[element_index];
            if let Some(owner) = owners.get(&element.markup_start) {
                if let Some(owner) = owner {
                    let owner_range = structure.query_range(&structure.nodes[*owner]);
                    if owner_range.start <= range.start && owner_range.end >= range.end {
                        let id = &structure.nodes[*owner].id;
                        if node.parent_id.as_ref() != Some(id) {
                            parents.push((index, id.clone()));
                        }
                    }
                }
                // A nearer typed owner is a boundary even if its body excludes
                // the child's printed marker or its source identity is ambiguous.
                break;
            }
            parent = element.parent;
        }
    }
    for (index, parent) in parents {
        structure.nodes[index].parent_id = Some(parent);
    }
}

pub struct CourtListenerSourceMarkup<'a> {
    pub kind: &'static str,
    pub markup_start: usize,
    pub markup: &'a str,
}

/// Exact served headmatter and opinion slices with original UTF-8 byte starts. Parsing
/// serialized HTML back through an HTML5 DOM can discard a leading PRE newline;
/// these slices preserve the browser serialization's original text bytes.
pub fn courtlistener_source_markups(
    markup: &str,
) -> Result<Vec<CourtListenerSourceMarkup<'_>>, EngineError> {
    let rendered = render_markup(DocumentProvider::CourtListener, markup, &[])?;
    Ok(rendered
        .elements
        .iter()
        .filter_map(|element| {
            let kind = if element
                .attributes
                .get("id")
                .is_some_and(|id| id == "headmatter")
            {
                "headmatter"
            } else if element.attributes.get("class").is_some_and(|classes| {
                classes
                    .split_ascii_whitespace()
                    .any(|class| class == "subopinion-content")
            }) {
                "opinion"
            } else {
                return None;
            };
            Some(CourtListenerSourceMarkup {
                kind,
                markup_start: element.markup_start,
                markup: &markup[element.markup_start..element.markup_end],
            })
        })
        .collect())
}

/// The inference exclusions a CourtListener rendition's evidence carries, in the final UTF-16
/// coordinates of the retained markup graph: (headings, paragraphs).
pub(super) fn inference_exclusions(evidence: &DocumentInput) -> (Vec<ScalarRange>, Vec<ScalarRange>) {
    let coordinates = ScalarText::new(&evidence.text);
    let mut headings = Vec::new();
    let mut body = Vec::new();
    for exclusion in &evidence.exclusions {
        let range = ScalarRange {
            start: coordinates.utf16(exclusion.range.start),
            end: coordinates.utf16(exclusion.range.end),
        };
        if exclusion.applies_to.iter().any(|kind| kind == "heading") {
            headings.push(range);
        }
        if exclusion.applies_to.iter().any(|kind| kind == "paragraph") {
            body.push(range);
        }
    }
    (headings, body)
}

/// A CourtListener rendition's reading after derivation: source headings and notes preferred, notes
/// paired with their references, and every pass over what the rendition supplies.
pub(super) fn finish(
    structure: &mut DocumentStructure,
    note_references: Vec<(String, Option<String>, ScalarRange)>,
    matched_role_starts: &HashSet<usize>,
    printed_notes: Vec<(RawBlock, Vec<NoteReference>)>,
    heading_exclusions: &[ScalarRange],
    body_exclusions: &[ScalarRange],
) {
    prefer_source_headings(structure);
    prefer_source_note_markers(structure);
    clip_courtlistener_paragraphs_to_opinions(structure, matched_role_starts);
    native_table_coordinates(structure);
    let mut references = HashMap::<String, Vec<NoteReference>>::new();
    for (target, anchor, range) in note_references {
        references.entry(target).or_default().push(NoteReference {
            range,
            anchor,
            page_indexes: Vec::new(),
            line_ids: Vec::new(),
        });
    }
    let mut anchor_counts = HashMap::<&str, usize>::new();
    for node in &structure.nodes {
        if let Some(anchor) = node.anchor.as_deref() {
            *anchor_counts.entry(anchor).or_default() += 1;
        }
    }
    let mut derived_notes = std::mem::take(&mut structure.notes)
        .into_iter()
        .filter_map(|note| note.node_id.clone().map(|id| (id, note)))
        .collect::<HashMap<_, _>>();
    structure.notes = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Footnote)
        .map(|node| {
            if let Some(note) = derived_notes.remove(&node.id) {
                return note;
            }
            let references = node
                .anchor
                .as_deref()
                .filter(|anchor| anchor_counts.get(anchor) == Some(&1))
                .and_then(|anchor| references.remove(anchor))
                .unwrap_or_default();
            Note {
                id: format!("note-{}", node.id),
                node_id: Some(node.id.clone()),
                kind: NoteKindV2::Footnote,
                // Attribute-only labels have no rendered span; a source
                // label or sibling number retains its separate range.
                label_range: node.marker_range.unwrap_or(ScalarRange {
                    start: node.range.start,
                    end: node.range.start,
                }),
                body_range: node.range,
                label_ranges: node.marker_range.into_iter().filter(|range| range.start < range.end).collect(),
                body_ranges: if node.ranges.is_empty() { vec![node.range] } else { node.ranges.clone() },
                primary_reference: references.first().map(|reference| reference.range),
                references,
            }
        })
        .collect();
    supplement_courtlistener_bookmarked_notes(structure);
    supplement_courtlistener_declared_note_references(structure, matched_role_starts);
    clip_courtlistener_pre_note_bodies(structure);
    crate::inference::supplement_source_formatted_headings(
        structure,
        heading_exclusions,
        body_exclusions,
    );
    supplement_courtlistener_printed_notes(structure, printed_notes);
    drop_note_numbered_paragraphs(structure);
    courtlistener_source_parents(structure);
    courtlistener_printed_star_pages(structure);
    courtlistener_slip_notes(structure);
    courtlistener_opening_pages(structure);
    courtlistener_resource_paragraph_markers(structure);
    courtlistener_bracketnum_divisions(structure);
    courtlistener_division_headings(structure);
    courtlistener_source_roles(structure);
}

/// What a CourtListener rendition's markup declares beyond the generic reading, in order: notes no
/// container holds, declared role blocks and recovered bodies, note labels by number or by sibling,
/// and printed notes (returned, to be added after derivation).
#[allow(clippy::too_many_arguments)]
pub(super) fn evidence(
    markup: Option<&str>,
    scope: &NativeMarkupScope,
    elements: &[MarkupElement],
    text: &str,
    rendered_blocks: &mut Vec<RawBlock>,
    rendered_exclusions: &mut Vec<ScalarRange>,
    mut recovered_body_blocks: Vec<RawBlock>,
    matched_role_starts: &HashSet<usize>,
) -> Vec<(RawBlock, Vec<NoteReference>)> {
    if scope.kind == "complete"
        && matches!(
            scope.markup_serialization,
            MarkupSerialization::HtmlSource | MarkupSerialization::BrowserHtml
        )
    {
        if let Some(markup) = markup {
            let notes =
                courtlistener_uncontained_note_blocks(markup, elements, text, rendered_blocks);
            for note in &notes {
                if let Some(marker) = note.metadata.note_marker {
                    rendered_exclusions.push(ScalarRange {
                        start: marker.start,
                        end: note.end,
                    });
                }
            }
            rendered_blocks.extend(notes);
        }
    }
    {
        // Declared roles use the existing rendered source ranges. Creating
        // them while rendering would add breaks to previously retained text.
        for block in rendered_blocks.iter_mut() {
            if block.kind == Kind::Prose
                && block.metadata.start.is_some_and(|start| {
                    matched_role_starts.contains(&start)
                        && elements
                            .binary_search_by_key(&start, |element| element.markup_start)
                            .ok()
                            .is_some_and(|index| {
                                let element = &elements[index];
                                element.tag == "p"
                                    && element
                                        .attributes
                                        .get("data-type")
                                        .is_some_and(|role| role == "header")
                            })
                })
            {
                // Preserve this claim's identity, anchor, printed text and children.
                block.kind = Kind::Heading;
            }
        }
        let declared = courtlistener_declared_blocks(
            elements,
            rendered_blocks,
            &recovered_body_blocks,
            matched_role_starts,
            text,
        );
        rendered_blocks.extend(declared);
        recovered_body_blocks.sort_by_key(|block| block.metadata.start);
        rendered_blocks.extend(recovered_body_blocks);
    }
    let numbered_note_markers = courtlistener_numbered_note_markers(elements, matched_role_starts, text);
    let sibling_note_markers = courtlistener_sibling_note_markers(elements, text);
    for block in rendered_blocks.iter_mut() {
        if block.kind == Kind::Footnote {
            if let Some((label, aliases, marker)) = block
                .metadata
                .start
                .and_then(|start| numbered_note_markers.get(&start))
            {
                if block.label.is_empty() || block.label == *label {
                    block.label = label.clone();
                    block.aliases = aliases.clone();
                    block.metadata.note_marker = Some(*marker);
                    block.start = marker.end;
                }
            }
            if let Some((label, _)) = block
                .metadata
                .start
                .and_then(|start| sibling_note_markers.get(&start))
            {
                block.label = label.clone();
            }
        }
    }
    let coordinates = ScalarText::new(text);
    if scope.kind == "complete" {
        markup.map(|markup| {
            courtlistener_printed_note_blocks(markup, elements, &coordinates, rendered_blocks)
        }).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// The labels a note's sibling number gives it (Westlaw's co_footnoteNumber), for a rendition of
/// another provider read with CourtListener's reader present.
pub(super) fn sibling_note_labels(elements: &[MarkupElement], text: &str, rendered_blocks: &mut [RawBlock]) {
    let sibling_note_markers = courtlistener_sibling_note_markers(elements, text);
    for block in rendered_blocks.iter_mut() {
        if block.kind == Kind::Footnote {
            if let Some((label, _)) = block
                .metadata
                .start
                .and_then(|start| sibling_note_markers.get(&start))
            {
                block.label = label.clone();
            }
        }
    }
}
