//! CourtListener's own renditions (html_with_citations, CAP's xml_harvard, Columbia, Lawbox, Lexis,
//! Westlaw, OSCN, public.resource.org): what their markup and printed text supply beyond the generic reading.

use super::*;
use crate::Derivation;
use crate::text::ScalarCoordinates;

/// Each element's nearest owner, itself or an ancestor, that `owns`: none where no such element holds it.
fn nearest_owners(elements: &[MarkupElement], owns: impl Fn(&MarkupElement) -> bool) -> Vec<Option<usize>> {
    let mut owners = Vec::with_capacity(elements.len());
    for (index, element) in elements.iter().enumerate() {
        let owner = if owns(element) { Some(index) } else { element.parent.and_then(|parent| owners[parent]) };
        owners.push(owner);
    }
    owners
}

/// The words over `range` without the whitespace around them, and where those words stand.
fn printed<'a>(text: &ScalarText<'a>, range: ScalarRange) -> Option<(&'a str, ScalarRange)> {
    let raw = text.slice_utf16(range.start..range.end)?;
    let value = trim_js(raw);
    let start = range.start + utf16_len(&raw[..raw.len() - raw.trim_start_matches(javascript_whitespace).len()]);
    Some((value, ScalarRange { start, end: start + utf16_len(value) }))
}

/// `value` as `key`'s one owner: a second claim, whatever it claims, leaves the key with none.
fn unique<K: Eq + std::hash::Hash, V>(owners: &mut HashMap<K, Option<V>>, key: K, value: Option<V>) {
    owners.entry(key).and_modify(|owner| *owner = None).or_insert(value);
}

/// Each id, and each anchor's name, by the one element that bears it (none where two do).
fn named_elements(elements: &[MarkupElement]) -> HashMap<&str, Option<usize>> {
    let mut named = HashMap::<&str, Option<usize>>::new();
    for (index, element) in elements.iter().enumerate() {
        let name = element.is("a").then(|| element.attributes.get("name")).flatten();
        for name in [element.attributes.get("id"), name].into_iter().flatten().filter(|name| !name.is_empty()) {
            named.entry(name.as_str()).and_modify(|owner| if *owner != Some(index) { *owner = None }).or_insert(Some(index));
        }
    }
    named
}

/// A writing of the opinion's the markup declares: an opinion, any writing's text (CAP's opinion_text, concurrence_text,
/// dissent_text and the like), or a block whose class names one (Westlaw's co_opinionBlock, LDML's, Decisia's, the opinion class).
pub(crate) fn declared_writing(element: &MarkupElement) -> bool {
    element.is("opinion")
        || element.tag.get(element.tag.len().saturating_sub(5)..).is_some_and(|tail| tail.eq_ignore_ascii_case("_text"))
        || (element.is("div") && element.classes().any(|class| matches!(class, "ldml-opinion" | "de-writing" | "co_opinionBlock")))
        || (["article", "section", "div"].iter().any(|tag| element.is(tag)) && element.has_class("opinion"))
}


pub(super) fn courtlistener_sibling_note_markers(
    elements: &[MarkupElement],
    coordinates: &ScalarText<'_>,
) -> HashMap<usize, (String, ScalarRange)> {
    let mut numbers = HashMap::<usize, Option<(String, ScalarRange)>>::new();
    let mut bodies = HashMap::<usize, Option<usize>>::new();
    for element in elements.iter().filter(|element| element.tag == "div") {
        let Some(parent) = element.parent else {
            continue;
        };
        if element.has_class("co_footnoteBody") {
            unique(&mut bodies, parent, Some(element.markup_start));
        }
        if !element.has_class("co_footnoteNumber") {
            continue;
        }
        let Some((label, range)) = printed(coordinates, element.range).filter(|(label, _)| short_number(label)) else {
            continue;
        };
        unique(&mut numbers, parent, Some((format!("{}", label.parse::<u32>().unwrap()), range)));
    }
    bodies
        .into_iter()
        .filter_map(|(parent, body)| Some((body?, numbers.remove(&parent)??)))
        .collect()
}

pub(super) fn courtlistener_footnote_number(raw: &str) -> Option<StructureNode> {
    let marker = trim_js(raw);
    let number = marker.strip_prefix("[fn")?.strip_suffix(']').filter(|number| short_number(number))?;
    let mut identity = footnote_identity(number, "", None)?;
    identity.aliases = Some(vec![marker.to_owned(), format!("footnote {marker}")]);
    Some(identity)
}

pub(super) fn courtlistener_numbered_note_markers(
    elements: &[MarkupElement],
    matched_role_starts: &HashSet<usize>,
    coordinates: &ScalarText<'_>,
) -> HashMap<usize, (Option<String>, Option<Vec<String>>, ScalarRange)> {
    if !elements
        .iter()
        .any(|element| element.tag == "footnote_number")
    {
        return HashMap::new();
    }
    let mut markers = HashMap::<usize, Option<(Option<String>, Option<Vec<String>>, ScalarRange)>>::new();
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
            let (printed, range) = printed(coordinates, element.range)?;
            let identity = courtlistener_footnote_number(printed)?;
            if body.range.end < range.end || !trim_js(coordinates.slice_utf16(body.range.start..range.start)?).is_empty() {
                return None;
            }
            Some((identity.label, identity.aliases, range))
        })();
        // A merged body with multiple declared numbers supplies no unique marker.
        unique(&mut markers, body.markup_start, marker);
    }
    markers
        .into_iter()
        .filter_map(|(start, marker)| Some((start, marker?)))
        .collect()
}

pub(super) fn supplement_courtlistener_declared_note_references(
    structure: &mut DocumentStructure,
    text_index: &ScalarCoordinates,
    matched_role_starts: &HashSet<usize>,
) {
    if !structure.nodes.iter().any(|node| node.kind == NodeKind::Footnote) {
        return;
    }
    let updates = {
        let elements = &structure.markup_elements;
        let matched = |element: &MarkupElement| matched_role_starts.contains(&element.markup_start);
        let ldml_reference = |element: &MarkupElement| element.tag == "sup" && element.has_class("ldml-superscript") && matched(element);
        let has_ldml_references = elements.iter().any(ldml_reference);
        let has_numbered_references = elements.iter().any(|element| element.tag == "footnote_reference");
        let has_references = elements.iter().any(|element| element.tag == "footnote_reference" || element.is("footnotereference"));
        if !has_ldml_references && !has_references && !elements.iter().any(|element| element.tag == "footnotemark") {
            return;
        }
        let opinion_owner = |index: usize| ancestors(elements, index).find(|&at| elements[at].tag == "opinion");
        let in_note = |index: usize| ancestors(elements, index).any(|at| elements[at].tag == "footnote");
        // Each opinion's notes by label, in source order: a label can repeat ("*", or a numbering restarted).
        let mut owners = HashMap::<(usize, &str), Vec<usize>>::new();
        for (index, element) in elements.iter().enumerate().filter(|(_, element)| element.tag == "footnote") {
            let (Some(opinion), Some(label)) = (opinion_owner(index), element.attributes.get("label").map(|label| trim_js(label))) else {
                continue;
            };
            if !label.is_empty() {
                owners.entry((opinion, label)).or_default().push(index);
            }
        }
        // Each note node by the markup it starts at.
        let notes = structure
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == NodeKind::Footnote)
            .filter_map(|(index, node)| Some((node.markup_start?, index)))
            .collect::<HashMap<_, _>>();
        let mut direct_targets = HashMap::<&str, Option<usize>>::new();
        if has_references {
            let mut identities = HashMap::<&str, Option<usize>>::new();
            for (index, element) in elements.iter().enumerate() {
                if let Some(id) = element.attributes.get("id").filter(|id| !id.is_empty()) {
                    unique(&mut identities, id.as_str(), Some(index));
                }
            }
            for (note, node) in structure.nodes.iter().enumerate().filter(|(_, node)| node.source == Derivation::Native && node.kind == NodeKind::Footnote) {
                let Some(index) = node.markup_start.and_then(|start| element_at(elements, start)) else {
                    continue;
                };
                let element = &elements[index];
                let Some(id) = element.attributes.get("id") else {
                    continue;
                };
                if identities.get(id.as_str()).copied().flatten() == Some(index)
                    && node.markup_tag.as_deref().is_some_and(|tag| element.is(tag))
                {
                    unique(&mut direct_targets, id.as_str(), Some(note));
                }
            }
        }
        let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
        let mut numbered_targets = HashMap::<&str, Option<usize>>::new();
        if has_numbered_references {
            for element in elements.iter().filter(|element| element.tag == "footnote_number") {
                let Some((label, range)) = printed(&coordinates, element.range).filter(|(label, _)| courtlistener_footnote_number(label).is_some()) else {
                    continue;
                };
                let target = element
                    .parent
                    .filter(|&parent| elements[parent].tag == "footnote_body" && matched(&elements[parent]) && matched(element))
                    .and_then(|parent| notes.get(&elements[parent].markup_start).copied())
                    .filter(|&note| structure.nodes[note].marker_range == Some(range));
                // Repeated declarations, including an unqualified second body, cannot establish a unique whole-field
                // reference relationship.
                unique(&mut numbered_targets, label, target);
            }
        }
        let is_ldml = |element: &MarkupElement, class: &str| element.tag == "div" && element.has_class(class);
        let mut decision_owners = Vec::<Option<usize>>::new();
        let mut writing_owners = Vec::<Option<usize>>::new();
        let mut writings = HashMap::<usize, Option<usize>>::new();
        let mut footers = HashMap::<usize, Option<usize>>::new();
        if has_ldml_references {
            for (index, element) in elements.iter().enumerate() {
                let owner = |owners: &[Option<usize>], class: &str| {
                    element.parent.and_then(|parent| if is_ldml(&elements[parent], class) { Some(parent) } else { owners[parent] })
                };
                let decision = owner(&decision_owners, "ldml-decision");
                writing_owners.push(owner(&writing_owners, "ldml-opinion"));
                decision_owners.push(decision);
                if let Some(decision) = decision {
                    // Even an unmatched or indirect second declaration is ambiguous.
                    if is_ldml(element, "ldml-opinion") {
                        unique(&mut writings, decision, Some(index));
                    } else if is_ldml(element, "ldml-notes") {
                        unique(&mut footers, decision, Some(index));
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
                && [decision, writing, footer].iter().all(|&index| matched(&elements[index])))
            .then_some((writing, footer))
        };
        let mut ldml_targets = HashMap::<(usize, &str), Option<usize>>::new();
        if has_ldml_references {
            let native_owners = structure
                .nodes
                .iter()
                .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote)
                .filter_map(|node| Some((node.markup_start?, (NodeKind::Footnote, node.range.start, node.range.end))))
                .collect::<HashMap<_, _>>();
            for (index, element) in elements.iter().enumerate() {
                if !courtlistener_ldml_note(element) {
                    continue;
                }
                let Some(decision) = decision_owners[index] else {
                    continue;
                };
                let Some((_, marker)) = courtlistener_declared_note_marker(elements, index, &coordinates, &native_owners) else {
                    continue;
                };
                let Some(label) = coordinates.slice_utf16(marker.start..marker.end).filter(|label| courtlistener_ldml_bracketed_marker(label)) else {
                    continue;
                };
                let target = ldml_scope(decision).and_then(|(_, footer)| {
                    let note = notes.get(&element.markup_start).copied()?;
                    let body = &structure.nodes[note];
                    (element.parent == Some(footer)
                        && writing_owners[index].is_none()
                        && matched(element)
                        && native_owners.contains_key(&element.markup_start)
                        && body.marker_range == Some(marker)
                        && body.range.start == marker.end
                        && body.range.end == element.range.end
                        && body.calls.is_empty()
                        && elements[index + 1..]
                            .iter()
                            .take_while(|child| child.markup_start < element.markup_end)
                            .any(|child| ldml_reference(child) && child.range == marker))
                    .then_some(note)
                });
                // Count literal labels even when a body cannot supply a qualified Note.
                unique(&mut ldml_targets, (decision, label), target);
            }
        }
        // A label's marks outside the notes, by opinion, in source order: where an opinion repeats a label, its marks pair
        // with its notes in order, the k-th mark with the k-th note, when they count the same.
        let mark = |index: usize| {
            let element = &elements[index];
            (element.tag == "footnotemark" && !in_note(index)).then(|| Some((opinion_owner(index)?, printed(&coordinates, element.range)?))).flatten()
        };
        let mut mark_counts = HashMap::<(usize, &str), usize>::new();
        for (opinion, (label, _)) in (0..elements.len()).filter_map(mark) {
            *mark_counts.entry((opinion, label)).or_default() += 1;
        }
        let mut marks_seen = HashMap::<(usize, &str), usize>::new();
        let mut updates = Vec::new();
        for (index, element) in elements.iter().enumerate() {
            if ldml_reference(element) {
                if ["id", "name", "href", "anchoridref"].iter().any(|key| element.attributes.contains_key(*key))
                    || elements.get(index + 1).is_some_and(|child| child.markup_start < element.markup_end)
                {
                    continue;
                }
                let Some(decision) = decision_owners[index] else {
                    continue;
                };
                let Some((writing, _)) = ldml_scope(decision) else {
                    continue;
                };
                let blocked = ancestors(elements, index).any(|at| {
                    let ancestor = &elements[at];
                    matches!(ancestor.tag.as_str(), "blockquote" | "table" | "nav" | "script" | "style" | "a")
                        || [
                            "ldml-note", "ldml-notes", "ldml-referencenote", "ldml-referencechain", "ldml-refname", "ldml-cite",
                            "ldml-headmatter", "ldml-header", "ldml-quotation",
                        ]
                        .iter()
                        .any(|class| ancestor.has_class(class))
                });
                if writing_owners[index] != Some(writing) || blocked {
                    continue;
                }
                let Some(label) = coordinates.slice_utf16(element.range.start..element.range.end).filter(|label| courtlistener_ldml_bracketed_marker(label)) else {
                    continue;
                };
                if let Some(note) = ldml_targets.get(&(decision, label)).copied().flatten() {
                    // This is an inferred printed-marker pair, not an explicit source link.
                    updates.push((note, element.range));
                }
            } else if element.tag == "footnote_reference" || element.is("footnotereference") {
                let Some((label, range)) = printed(&coordinates, element.range).filter(|(label, _)| !label.is_empty()) else {
                    continue;
                };
                let target = if element.tag == "footnote_reference" {
                    matched(element).then(|| numbered_targets.get(label).copied().flatten()).flatten()
                } else {
                    element.attributes.get("anchoridref").and_then(|target| direct_targets.get(target.as_str())).copied().flatten()
                };
                if let Some(note) = target {
                    updates.push((note, range));
                }
            } else if let Some((opinion, (label, range))) = mark(index) {
                let ordinal = {
                    let seen = marks_seen.entry((opinion, label)).or_default();
                    *seen += 1;
                    *seen - 1
                };
                let owner = owners.get(&(opinion, label)).and_then(|owned| match owned.len() {
                    1 => Some(owned[0]),
                    count if mark_counts.get(&(opinion, label)) == Some(&count) => owned.get(ordinal).copied(),
                    _ => None,
                });
                if let Some(&note) = owner.and_then(|owner| notes.get(&elements[owner].markup_start)) {
                    updates.push((note, range));
                }
            }
        }
        updates
    };
    for (index, range) in updates {
        // IDREF markers retain only their own actual unique ID, if any. Label-only same-opinion relationships supply no
        // source fragment.
        let note = &mut structure.nodes[index];
        if !note.calls.contains(&range) {
            note.calls.push(range);
        }
    }
}

pub(super) fn clip_courtlistener_pre_note_bodies(structure: &mut DocumentStructure, text_index: &ScalarCoordinates) {
    let ends = {
        let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
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
            let mut lines = raw.split_inclusive('\n');
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
        structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote)
            .filter(|note| note.source == Derivation::Heuristic && !note.calls.is_empty())
            .filter_map(|note| {
                let label = note_label(note);
                let number = coordinates.slice_utf16(label.start..label.end)?.parse::<u32>().ok()?;
                let owner = pre.iter().find(|element| element.range.start <= label.start && label.end <= element.range.end)?;
                let prefix = coordinates.slice_utf16(owner.range.start..label.start)?;
                let prefix = prefix.rsplit('\n').next()?;
                if !prefix.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
                    return None;
                }
                let end = boundaries
                    .iter()
                    .filter(|(next, start, indent)| {
                        *next == number + 1
                            && *indent == prefix.len()
                            && note.range.start < *start
                            && *start < note.range.end
                    })
                    .map(|(_, start, _)| *start)
                    .min()?;
                Some((note.id.clone(), end))
            })
            .collect::<HashMap<_, _>>()
    };
    // PRE container starts can expose a printed next label even when HTML's
    // opening-LF removal joins it to the preceding rendered line. Bound only an
    // existing corroborated fragment; the next label supplies no new note/ref.
    for note in &mut structure.nodes {
        if let Some(end) = ends.get(&note.id) {
            note.range.end = *end;
            note.ranges.retain_mut(|range| {
                range.end = range.end.min(*end);
                range.start < range.end
            });
        }
    }
}

pub(super) fn supplement_courtlistener_bookmarked_notes(structure: &mut DocumentStructure, text_index: &ScalarCoordinates, sibling_markers: &HashMap<usize, (String, ScalarRange)>) {
    if !structure.nodes.iter().any(|node| node.kind == NodeKind::Footnote) {
        return;
    }
    let (updates, printed_updates) = {
        let elements = &structure.markup_elements;
        let has_fn_notes = elements.iter().any(|element| element.tag == "div" && element.has_class("fn-footnote"));
        let reference_span = |element: &MarkupElement| element.is("sup") || (element.is("span") && element.has_class("MsoFootnoteReference"));
        if !has_fn_notes
            && !elements.iter().any(|element| {
                (element.is("a") && element.attributes.get("name").is_some_and(|name| name.starts_with("_ftn")))
                    || (element.is("sup")
                        && element.attributes.get("id").is_some_and(|id| id.starts_with("fn") || id.starts_with("op") || id.starts_with("co_fnRef_")))
                    || (element.is("span") && element.has_class("MsoFootnoteReference"))
                    || (element.is("div") && element.attributes.get("id").is_some_and(|id| id.starts_with("ftn")))
            })
        {
            return;
        }
        let reference_spans = elements.iter().enumerate().filter(|(_, element)| reference_span(element)).map(|(index, _)| index).collect::<Vec<_>>();
        let identities = named_elements(elements);
        let note_starts = structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote).filter_map(|node| node.markup_start).collect::<HashSet<_>>();
        let note_owner = |index: usize| ancestors(elements, index).map(|at| elements[at].markup_start).find(|start| note_starts.contains(start));
        let parent_tag = |index: usize, tag: &str| ancestors(elements, index).find(|&at| elements[at].is(tag));
        let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
        // Where an element prints `expected`, bare or bracketed.
        let printed_range = |index: usize, expected: &str| {
            printed(&coordinates, elements[index].range)
                .filter(|(printed, _)| *printed == expected || *printed == format!("[{expected}]"))
                .map(|(_, range)| range)
        };
        // The writings the markup declares and the reader took as sections: an opinion's own (its tag, or Westlaw's and
        // the opinion class), and a writing within one (its text, a concurrence's or dissent's, LDML's or Decisia's).
        let section = |node: &&StructureNode, tags: &[&str], classes: &[&str]| {
            node.source == Derivation::Native
                && node.kind == NodeKind::Section
                && (node.markup_tag.as_deref().is_some_and(|tag| tags.contains(&tag))
                    || node.markup_attributes.get("class").is_some_and(|names| names.split_ascii_whitespace().any(|class| classes.contains(&class))))
        };
        let starts = |tags: &[&str], classes: &[&str]| {
            structure.nodes.iter().filter(|node| section(node, tags, classes)).filter_map(|node| node.markup_start).collect::<HashSet<_>>()
        };
        // Source ownership, rather than Word's formatting sections, bounds an anonymous pair. A nested native opinion starts
        // a distinct owner; an element under none is its outermost ancestor's.
        let opinion_starts = starts(&["opinion"], &["co_opinionBlock", "opinion"]);
        let mut document_owners = Vec::with_capacity(elements.len());
        for (index, element) in elements.iter().enumerate() {
            let owner = if opinion_starts.contains(&element.markup_start) { index } else { element.parent.map_or(index, |parent| document_owners[parent]) };
            document_owners.push(owner);
        }
        let anonymous_markers = reference_spans
            .iter()
            .copied()
            .filter(|&index| {
                let element = &elements[index];
                let superscript = element.is("sup");
                if parent_tag(index, "a").is_some()
                    || ancestors(elements, index).any(|at| elements[at].has_class("MsoFootnoteReference") || (superscript && elements[at].is("sup")))
                {
                    return false;
                }
                std::iter::once(element)
                    .chain(elements[index + 1..].iter().take_while(|child| child.markup_start < element.markup_end))
                    .enumerate()
                    .all(|(position, child)| {
                        !child.is("a")
                            && !["id", "href", "name"].iter().any(|key| child.attributes.contains_key(*key))
                            && (!superscript || position == 0 || (!child.is("sup") && !child.has_class("MsoFootnoteReference")))
                    })
            })
            .collect::<Vec<_>>();
        let mut native_body_counts = HashMap::<(usize, &str), usize>::new();
        for node in structure.nodes.iter().filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote) {
            if let (Some(index), Some(label)) = (node.markup_start.and_then(|start| element_at(elements, start)), node.label.as_deref()) {
                *native_body_counts.entry((document_owners[index], label)).or_default() += 1;
            }
        }
        let mut updates = Vec::new();
        let mut printed_updates = Vec::new();
        let fn_writings = if has_fn_notes {
            // A writing the markup declares (`declared_writing`) that the reader took as a section.
            let writing_starts = structure
                .nodes
                .iter()
                .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Section)
                .filter_map(|node| node.markup_start)
                .collect::<HashSet<_>>();
            nearest_owners(elements, |element| writing_starts.contains(&element.markup_start) && declared_writing(element))
        } else {
            Vec::new()
        };
        // A superscript number, read as a note's label.
        let numeric_marker = |index: usize| {
            if elements[index].tag != "sup" {
                return None;
            }
            let (printed, _) = printed(&coordinates, elements[index].range).filter(|(printed, _)| short_number(printed))?;
            Some((footnote_identity(printed, "", None)?, printed_range(index, printed)?, printed))
        };
        // Decisia's notes (div.fn-footnote): the superscript number that opens each, where it agrees with what the note declares.
        let mut fn_markers = HashMap::new();
        if has_fn_notes {
            let fn_notes = structure
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == NodeKind::Footnote
                        && node.source == Derivation::Native
                        && node.markup_tag.as_deref() == Some("div")
                        && node.markup_attributes.get("class").is_some_and(|names| names.split_ascii_whitespace().any(|class| class == "fn-footnote"))
                })
                .filter_map(|node| Some((node.markup_start?, node)))
                .collect::<HashMap<_, _>>();
            for &index in &anonymous_markers {
                let Some((owner, node)) = note_owner(index).and_then(|owner| Some((owner, *fn_notes.get(&owner)?))) else {
                    continue;
                };
                let Some((identity, range, printed)) = numeric_marker(index) else {
                    continue;
                };
                let declared_otherwise = ["data-label", "label", "n"].iter().any(|key| {
                    node.markup_attributes
                        .get(*key)
                        .filter(|raw| !trim_js(raw).is_empty())
                        .is_some_and(|raw| footnote_identity(raw, "", None).is_none_or(|declared| declared.label != identity.label))
                });
                if coordinates.slice_utf16(node.range.start..range.start).is_some_and(|prefix| trim_js(prefix).is_empty())
                    && (node.marker_range.is_none() || node.marker_range == Some(range))
                    && !declared_otherwise
                {
                    unique(&mut fn_markers, owner, Some((identity, range, printed)));
                }
            }
        }
        let mut fn_body_counts = HashMap::new();
        if has_fn_notes {
            for node in structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote && node.source == Derivation::Native) {
                let Some(start) = node.markup_start else {
                    continue;
                };
                let label = fn_markers.get(&start).and_then(Option::as_ref).map_or(node.label.as_deref(), |(identity, _, _)| identity.label.as_deref());
                if let (Some(index), Some(label)) = (element_at(elements, start), label) {
                    *fn_body_counts.entry((fn_writings[index], label)).or_insert(0usize) += 1;
                }
            }
        }
        for (note_index, note) in structure.nodes.iter().enumerate().filter(|(_, node)| node.kind == NodeKind::Footnote) {
            let node = note;
            if let Some((identity, marker, printed)) = node.markup_start.and_then(|start| fn_markers.get(&start)).and_then(Option::as_ref) {
                let Some(body_index) = node.markup_start.and_then(|start| element_at(elements, start)) else {
                    continue;
                };
                let label = identity.label.as_deref().unwrap_or_default();
                if note_label(note).start != note_label(note).end && note.marker_range != Some(*marker) {
                    continue;
                }
                let mut references = Vec::new();
                let container = ancestors(elements, body_index).find(|&at| elements[at].has_class("fn-container"));
                if let Some(container) = container.filter(|_| note.calls.is_empty() && fn_body_counts.get(&(fn_writings[body_index], label)) == Some(&1)) {
                    for &index in &anonymous_markers {
                        if elements[index].markup_end > elements[container].markup_start
                            || note_owner(index).is_some()
                            || fn_writings[index] != fn_writings[body_index]
                        {
                            continue;
                        }
                        // Without a supplied writing, only its actual enclosing source container corroborates an anonymous
                        // reference.
                        if fn_writings[body_index].is_none()
                            && !elements[container].parent.is_some_and(|parent| ancestors(elements, index).any(|at| at == parent))
                        {
                            continue;
                        }
                        if let Some((_, range, _)) = numeric_marker(index).filter(|(reference, _, _)| reference.label.as_deref() == Some(label)) {
                            references.push(range);
                        }
                    }
                }
                printed_updates.push((note_index, *marker, label.to_owned(), (*printed).to_owned(), references));
                continue;
            }
            if let Some((label, range)) = node
                .markup_start
                .and_then(|start| sibling_markers.get(&start))
            {
                if node.label.as_ref() != Some(label)
                    || note.marker_range != Some(*range)
                    || !note.calls.is_empty()
                {
                    continue;
                }
                let Some(body_index) = element_at(elements, node.markup_start.unwrap())
                else {
                    continue;
                };
                let Some(parent) = elements[body_index].parent else {
                    continue;
                };
                let number = label.as_str();
                let mut markers = elements.iter().enumerate().filter_map(|(index, element)| {
                    let number_index = element.parent?;
                    let number_element = &elements[number_index];
                    let id = element.attributes.get("id")?;
                    let opaque = id.strip_prefix("co_footnote_")?;
                    (element.tag == "span"
                        && !opaque.is_empty()
                        && number_element.tag == "div"
                        && number_element.has_class("co_footnoteNumber")
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
                        .then_some(range)
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
            if note_label(note).start != note_label(note).end {
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
                let Some(number) = node.label.as_deref().filter(|number| {
                    *number == "*"
                        || (short_number(number))
                }) else {
                    continue;
                };
                let mut markers = elements.iter().enumerate().filter_map(|(index, element)| {
                    let id = element.attributes.get("id")?;
                    let scoped_label = id
                        .strip_prefix("op")
                        .and_then(|value| value.split_once('-'));
                    (element.tag == "sup"
                        && (id.strip_prefix("fn") == Some(number)
                            || scoped_label.is_some_and(|(opinion, label)| {
                                node.markup_tag.as_deref() == Some("div")
                                    && !opinion.is_empty()
                                    && opinion.bytes().all(|byte| byte.is_ascii_digit())
                                    && label.strip_prefix("fn") == Some(number)
                            }))
                        && identities.get(id.as_str()).copied().flatten() == Some(index)
                        && note_owner(index) == node.markup_start
                        && printed_range(index, number)
                            .is_some_and(|range| range.start == note.range.start))
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
                if !marker.is("sup")
                    || node.markup_start.is_none()
                    || note_owner(marker_index) != node.markup_start
                    || label_range.start != note.range.start
                    || !coordinates
                        .slice_utf16(label_range.end..note.range.end)
                        .is_some_and(|body| body.chars().any(char::is_alphabetic))
                {
                    continue;
                }
                updates.push((note_index, Some(label_range), None));
                if !note.calls.is_empty() {
                    continue;
                }
                let forward_href = format!("#{}", marker.attributes["id"]);
                if elements
                    .iter()
                    .filter(|element| {
                        element.is("a")
                            && element.attributes.get("href") == Some(&forward_href)
                    })
                    .count()
                    != 1
                {
                    continue;
                }
                let mut pairs = elements.iter().enumerate().filter_map(|(index, backlink)| {
                    if !backlink.is("a")
                        || backlink.parent != Some(marker_index)
                        || printed_range(index, number) != Some(label_range)
                    {
                        return None;
                    }
                    let reference_id = backlink.attributes.get("href")?.strip_prefix('#')?;
                    let reference_index = identities.get(reference_id).copied().flatten()?;
                    let reference = &elements[reference_index];
                    let range = printed_range(reference_index, number)?;
                    if !reference.is("sup")
                        || note_owner(reference_index).is_some()
                        || reference.markup_end > node.markup_start?
                        || elements
                            .iter()
                            .enumerate()
                            .filter(|(index, element)| {
                                element.is("a")
                                    && element.parent == Some(reference_index)
                                    && element.attributes.get("href") == Some(&forward_href)
                                    && printed_range(*index, number) == Some(range)
                            })
                            .count()
                            != 1
                    {
                        return None;
                    }
                    Some(range)
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
                short_number(number)
            }) else {
                continue;
            };
            let Some(owner_index) = identities.get(body_id).copied().flatten() else {
                continue;
            };
            let owner = &elements[owner_index];
            if !owner.is("div")
                || node.markup_start != Some(owner.markup_start)
            {
                continue;
            }
            let body_name = format!("_{body_id}");
            let reference_name = format!("_ftnref{number}");
            let owner_scope = document_owners[owner_index];
            if node.source == Derivation::Native
                && node.label.as_deref() == Some(number)
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
                    let printed_label = if elements[index].is("sup") {
                        number
                    } else {
                        printed.as_str()
                    };
                    (coordinates.slice_utf16(range.start..range.end) == Some(printed_label)
                        && note_owner(index) == Some(owner.markup_start)
                        && document_owners[index] == owner_scope
                        && range.start == note.range.start)
                        .then_some(range)
                });
                if let Some(label_range) = labels.next().filter(|range| {
                    coordinates
                        .slice_utf16(range.end..note.range.end)
                        .is_some_and(|body| body.chars().any(char::is_alphabetic))
                }) {
                    if labels.next().is_some() {
                        continue;
                    }
                    // A printed label is independent of whether the
                    // corresponding reference is absent or already known.
                    updates.push((note_index, Some(label_range), None));
                    if !note.calls.is_empty() {
                        continue;
                    }
                    let mut references = anonymous_markers.iter().filter_map(|&index| {
                        if !elements[index].is("span")
                            || !elements[index].has_class("MsoFootnoteReference")
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
                            updates.last_mut().unwrap().2 = Some(range);
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
            if !body_bookmark.is("a")
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
            if !elements[paragraph].is("p") {
                continue;
            }
            let printed = number;
            let mut labels = elements.iter().enumerate().filter_map(|(index, element)| {
                if !element.is("a")
                    || element.parent != Some(paragraph)
                    || note_owner(index) != Some(owner.markup_start)
                {
                    return None;
                }
                let mut superscripts =
                    elements.iter().enumerate().filter_map(|(sup_index, sup)| {
                        (sup.is("sup")
                            && parent_tag(sup_index, "a") == Some(index))
                        .then(|| printed_range(sup_index, printed))
                        .flatten()
                        .filter(|range| range.start == note.range.start)
                    });
                let superscript = superscripts.next();
                if superscripts.next().is_some() {
                    return None;
                }
                let range = superscript.or_else(|| {
                    elements[paragraph].has_class("MsoFootnoteText")
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
                || label_range.start != note.range.start
                || !coordinates
                    .slice_utf16(label_range.end..note.range.end)
                    .is_some_and(|body| body.chars().any(char::is_alphabetic))
            {
                continue;
            }
            // The leading declared label is independent of a reference's
            // printed text or href. Keep a conflict without losing the label.
            updates.push((note_index, Some(label_range), None));
            if !note.calls.is_empty() {
                continue;
            }
            let Some(reference_bookmark_index) =
                identities.get(reference_name.as_str()).copied().flatten()
            else {
                continue;
            };
            let reference_bookmark = &elements[reference_bookmark_index];
            if !reference_bookmark.is("a")
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
            updates.last_mut().unwrap().2 = Some(matches[0].1);
        }
        (updates, printed_updates)
    };
    for (index, label_range, reference) in updates {
        let note = &mut structure.nodes[index];
        if let Some(label_range) = label_range {
            note.marker_range = Some(label_range);
            note.range.start = label_range.end;
            note.ranges.retain_mut(|range| {
                range.start = range.start.max(label_range.end);
                range.start < range.end
            });
        }
        if let Some(reference_range) = reference {
            note.calls.push(reference_range);
        }
    }
    let mut markers = HashMap::new();
    for (index, marker, label, printed, references) in printed_updates {
        let note = &mut structure.nodes[index];
        note.marker_range = Some(marker);
        markers.insert(note.id.clone(), (marker, label, printed));
        if !references.is_empty() {
            note.calls = references;
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
    owners: &HashMap<usize, (NodeKind, usize, usize)>,
) -> Option<(StructureNode, ScalarRange)> {
    let note = &elements[index];
    let a_note = |element: &MarkupElement| {
        courtlistener_ldml_note(element) || owners.get(&element.markup_start).is_some_and(|(kind, _, _)| *kind == NodeKind::Footnote)
    };
    // The note's first superscript that no note within it holds.
    let superscript = (index + 1..elements.len())
        .take_while(|&at| elements[at].markup_start < note.markup_end)
        .find(|&at| elements[at].is("sup") && ancestors(elements, at).find(|&owner| owner == index || a_note(&elements[owner])) == Some(index))
        .map(|at| &elements[at])?;
    let (printed, printed_at) = printed(text, superscript.range)?;
    let (marker, start) = if printed.is_empty() {
        let raw = text.slice_utf16(note.range.start..note.range.end)?;
        let raw = raw.trim_start_matches(javascript_whitespace);
        let start = note.range.end - utf16_len(raw);
        if superscript.range.end > start {
            return None;
        }
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
        (printed, printed_at.start)
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
            a_note(element)
                && element.range.start < marker_range.end
                && marker_range.start < element.range.end
        })
    {
        return None;
    }
    let mut identity = footnote_identity(marker.strip_suffix('.').unwrap_or(marker), "", None)?;
    if marker.ends_with('.') {
        identity.aliases = Some(vec![marker.to_owned(), format!("footnote {marker}")]);
    }
    Some((identity, marker_range))
}

pub(super) fn courtlistener_declared_blocks(
    elements: &[MarkupElement],
    existing: &[StructureNode],
    recovered_bodies: &[StructureNode],
    matched_role_starts: &HashSet<usize>,
    text: &ScalarText<'_>,
) -> Vec<StructureNode> {
    let declared_role = |element: &MarkupElement| {
        let class = |name: &str| element.has_class(name);
        if courtlistener_ldml_note(element) && matched_role_starts.contains(&element.markup_start) {
            Some(NodeKind::Footnote)
        } else if (element.is("div")
            && (class("ldml-opinion") || class("ldml-section")))
            || (element.is("span") && class("ldml-appendix"))
            || (element.is("div")
                && class("de-writing")
                && matched_role_starts.contains(&element.markup_start))
            || (element.is("summary")
                && matched_role_starts.contains(&element.markup_start)
                && element.range.start < element.range.end
                && element
                    .parent
                    .is_some_and(|parent| elements[parent].is("opinion")))
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
            Some(NodeKind::Section)
        } else if (element.is("section") && class("ldml-heading"))
            || (element.tag == "p"
                && element
                    .attributes
                    .get("data-type")
                    .is_some_and(|role| role == "header")
                && matched_role_starts.contains(&element.markup_start))
        {
            Some(NodeKind::Heading)
        } else {
            None
        }
    };
    if !elements
        .iter()
        .any(|element| element.is("dl") || declared_role(element).is_some())
    {
        return Vec::new();
    }

    let mut owners = existing
        .iter()
        .chain(recovered_bodies)
        .filter_map(|block| Some((block.markup_start?, (block.kind, block.range.start, block.range.end))))
        .collect::<HashMap<_, _>>();
    let mut recovered = Vec::new();
    for (index, element) in elements.iter().enumerate() {
        let list = element.is("dl");
        let item = (element.is("dt")
            || element.is("dd"))
            && element.parent.is_some_and(|parent| {
                let owner = &elements[parent];
                owner.is("dl")
                    && owners
                        .get(&owner.markup_start)
                        .is_some_and(|&(kind, _, _)| kind == NodeKind::List)
            });
        let kind = if let Some(kind) = declared_role(element) {
            kind
        } else if list || item {
            NodeKind::List
        } else {
            continue;
        };
        if owners.contains_key(&element.markup_start) {
            continue;
        }
        let parent_id = ancestors(elements, index)
            .map(|at| elements[at].markup_start)
            .find(|start| {
                owners.get(start).is_some_and(|&(kind, from, to)| kind != NodeKind::Page && from <= element.range.start && to >= element.range.end)
            })
            .map(|start| format!("native-markup-{start}"));
        let attributes = element
            .attributes
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let anchor = attributes.get("id").filter(|id| !id.is_empty()).cloned();
        let note = (kind == NodeKind::Footnote)
            .then(|| {
                courtlistener_declared_note_marker(elements, index, text, &owners)
            })
            .flatten();
        let mut block = element_node(kind, String::new(), element);
        match note {
            Some((identity, marker)) => {
                (block.label, block.aliases, block.marker_range) = (identity.label, identity.aliases, Some(marker));
                block.range.start = marker.end;
            }
            None if kind != NodeKind::Footnote => block.label = anchor.clone(),
            None => {}
        }
        block.anchor = anchor;
        block.markup_attributes = attributes;
        block.markup_tag = Some(element.tag.to_ascii_lowercase());
        block.parent_id = parent_id;
        recovered.push(block);
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
    existing: &[StructureNode],
) -> Vec<(StructureNode, Vec<ScalarRange>)> {
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
        .filter(|block| block.kind == NodeKind::Footnote)
        .map(|block| block.range)
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
    let writings = nearest_owners(elements, declared_writing);
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^(\[(?:[0-9]{1,5}|\*{1,2})\])(?:\s|$)").unwrap());
    let printed = |element: &MarkupElement| {
        let raw = text.slice_utf16(element.range.start..element.range.end)?;
        let value = raw.trim_start_matches(javascript_whitespace);
        let marker = label.captures(value)?.get(1)?.as_str();
        let start = element.range.end - utf16_len(value);
        Some((marker, ScalarRange { start, end: start + utf16_len(marker) }))
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
                // An embedded quoted writing is continuation evidence, not a new sibling writing after the notes region.
                ancestors(elements, index).find(|&at| at == parent || matches!(elements[at].tag.as_str(), "p" | "blockquote" | "table")) == Some(parent)
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
        let mut references = HashMap::<&str, Vec<ScalarRange>>::new();
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
            if !ancestors(elements, index).any(|at| at == parent) {
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
            references.entry(marker).or_default().push(range);
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
            let mut note = element_node(NodeKind::Footnote, String::new(), element);
            (note.label, note.aliases, note.anchor) = (identity.label, identity.aliases, None);
            note.marker_range = Some(marker_range);
            note.range = ScalarRange { start: marker_range.end, end };
            result.push((
                note,
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
    notes: Vec<(StructureNode, Vec<ScalarRange>)>,
) {
    for (mut node, references) in notes {
        let Some(marker) = node.marker_range else {
            continue;
        };
        if structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote).any(|note| {
            note.marker_range == Some(marker)
                || (note.range.start <= marker.start && marker.end <= note.range.end)
        }) {
            continue;
        }
        // NOTES, a printed label and SUP corroborate a recovered role; the
        // ordinary source P remains Native and is not an explicit footnote tag.
        node.id = format!("heuristic-footnote-markup-{}", node.markup_start.unwrap());
        node.origin_id = crate::ENGINE_ORIGIN.into();
        node.source = Derivation::Heuristic;
        node.grammar = Some("courtlistener-notes-sup".to_owned());
        node.calls = references;
        structure.nodes.push(node);
    }
}

/// A printed note's label is not a paragraph number: a recovered numbered paragraph that a
/// note's own marker opens is that note, not the opinion's prose.
pub(super) fn drop_note_numbered_paragraphs(structure: &mut DocumentStructure) {
    let labels = structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote)
        .filter_map(|note| note.marker_range)
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
}

pub(super) fn courtlistener_uncontained_note_blocks(
    markup: &str,
    elements: &[MarkupElement],
    coordinates: &ScalarText<'_>,
    existing: &[StructureNode],
) -> Vec<StructureNode> {
    let candidates = elements
        .iter()
        .enumerate()
        .filter(|(_, element)| {
            element.is("a")
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
    let anchors = named_elements(elements);
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
    let descendant = |index: usize, ancestor: usize| ancestors(elements, index).any(|at| at == ancestor);
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
            short_number(number)
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
        if !paragraph.is("p")
            || inner(paragraph).is_none()
            || elements.iter().enumerate().any(|(index, element)| {
                element.is("p") && descendant(index, paragraph_index)
            })
            || existing.iter().any(|block| {
                block.markup_start == Some(note.markup_start)
                    || block.kind == NodeKind::Footnote
                        && block.range.start < paragraph.range.end
                        && paragraph.range.start < block.range.end
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
        if !marker.is("span")
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
            element.is("a")
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
            element.is("span")
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
        let ancestor_tags = std::iter::successors(last_paragraph.parent, |&at| elements[at].parent).map(|at| elements[at].tag.as_str()).collect::<Vec<_>>();
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
                element.is("p")
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
                element.is("hr")
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
                let mut block = element_node(NodeKind::Footnote, String::new(), note);
                (block.label, block.aliases, block.anchor) = (identity.label, identity.aliases, identity.anchor);
                block.marker_range = Some(marker);
                block.range = ScalarRange { start: marker.end, end: paragraph.range.end };
                result.push(block);
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
                block.markup_start == Some(note.markup_start)
                    || block.kind == NodeKind::Footnote
                        && block.range.start <= note.range.start
                        && note.range.end <= block.range.end
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
        if !reference.is("a")
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
        if !heading.is("div")
            || reference.markup_end >= heading.markup_start
            || coordinates
                .slice_utf16(heading.range.start..heading.range.end)
                .map(trim_js)
                != Some("Footnotes")
            || preceding[1..]
                .iter()
                .any(|element| !element.is("br"))
            || !gap(heading.markup_end, preceding[1].markup_start)
            || !gap(preceding[1].markup_end, preceding[2].markup_start)
            || !gap(preceding[2].markup_end, note.markup_start)
        {
            continue;
        }
        let following = &siblings[position + 1..];
        let Some(separator_at) = following
            .iter()
            .position(|element| element.is("br"))
        else {
            continue;
        };
        // ponytail: only the witnessed terminal single fragment; broader inline
        // layouts and continuations need their own independent source evidence.
        if following[..separator_at]
            .iter()
            .any(|element| !element.is("i"))
            || following.len() != separator_at + 5
        {
            continue;
        }
        let separators = &following[separator_at..separator_at + 4];
        let navigation = following[separator_at + 4];
        if separators
            .iter()
            .any(|element| !element.is("br"))
            || separators
                .windows(2)
                .any(|pair| !gap(pair[0].markup_end, pair[1].markup_start))
            || !gap(separators[3].markup_end, navigation.markup_start)
            || !navigation.is("div")
        {
            continue;
        }
        let Some(navigation_index) = element_at(elements, navigation.markup_start)
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
            || !form.is("form")
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
        let start = end - utf16_len(raw_body.trim_start_matches(javascript_whitespace));
        let marker_start = note.range.end - utf16_len(raw_label.trim_start_matches(javascript_whitespace));
        let Some(mut identity) = footnote_identity(&capture[1], "", Some(name.clone())) else {
            continue;
        };
        identity.aliases.get_or_insert_with(Vec::new).push(label.to_owned());
        let mut block = element_node(NodeKind::Footnote, String::new(), note);
        (block.label, block.aliases, block.anchor) = (identity.label, identity.aliases, identity.anchor);
        block.marker_range = Some(ScalarRange { start: marker_start, end: marker_start + utf16_len(label) });
        block.range = ScalarRange { start, end: start + utf16_len(body) };
        result.push(block);
    }
    result
}

pub(super) fn native_table_coordinates(structure: &mut DocumentStructure, text_index: &ScalarCoordinates) {
    let mut tables = structure
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Table && node.markup_start.is_some())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    tables.sort_by_key(|&index| structure.nodes[index].markup_start);
    let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
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
            unique(&mut headings, (range.start, range.end), Some(node.id.clone()));
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
}

pub(super) fn prefer_source_note_markers(structure: &mut DocumentStructure) {
    let mut markers = HashMap::new();
    for node in &structure.nodes {
        if node.kind == NodeKind::Footnote
            && node.source == Derivation::Native
            && node.markup_start.is_some()
        {
            if let Some(marker) = node.marker_range.filter(|range| range.start < range.end) {
                unique(&mut markers, marker.start, Some(node.id.clone()));
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
                unique(&mut native_scopes, start, Some(structure.query_range(node)));
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
        let declared_opinion = declared_writing(element);
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
        if element.is("p") && element.range.start < element.range.end {
            unique(&mut paragraph_scopes, element.range.start, scope.map(|scope| (scope, element.range.end, element.markup_end)));
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

/// An opinion's divisions printed as paragraphs of their own, "I", "II", "A.": headings, where
/// the opinion opens its divisions with "I".
pub(super) fn courtlistener_division_headings(structure: &mut DocumentStructure, text_index: &ScalarCoordinates) {
    static DIVISION: OnceLock<Regex> = OnceLock::new();
    let division = DIVISION.get_or_init(|| Regex::new(r"^(?:[IVX]{1,5}|[A-H])\.?$").unwrap());
    let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
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
pub(crate) fn cap_front_matter(typed: &str) -> Option<&'static str> {
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
        let at = element_at(elements, node.markup_start?)?;
        std::iter::once(at).chain(ancestors(elements, at)).find_map(|at| role(&elements[at])).map(|role| (index, role))
    }).collect::<Vec<_>>();
    for (index, role) in roles {
        structure.nodes[index].role = Some(role);
    }
}

/// A CourtListener rendition's reading after derivation: source headings and notes preferred, notes
/// paired with their references, and every pass over what the rendition supplies.
pub(super) fn finish(
    structure: &mut DocumentStructure,
    note_references: Vec<(String, ScalarRange)>,
    matched_role_starts: &HashSet<usize>,
    (printed_notes, sibling_note_markers): (Vec<(StructureNode, Vec<ScalarRange>)>, HashMap<usize, (String, ScalarRange)>),
    heading_exclusions: &[ScalarRange],
    body_exclusions: &[ScalarRange],
) {
    // The text is fixed through these passes: it is indexed once for all of them.
    let text_index = ScalarCoordinates::new(structure.query_text());
    prefer_source_headings(structure);
    prefer_source_note_markers(structure);
    clip_courtlistener_paragraphs_to_opinions(structure, matched_role_starts);
    native_table_coordinates(structure, &text_index);
    let mut references = HashMap::<String, Vec<ScalarRange>>::new();
    for (target, range) in note_references {
        references.entry(target).or_default().push(range);
    }
    // Each note the markup declares its calls, by its anchor where one node alone holds that anchor.
    let mut anchor_counts = HashMap::<String, usize>::new();
    for node in &structure.nodes {
        if let Some(anchor) = &node.anchor {
            *anchor_counts.entry(anchor.clone()).or_default() += 1;
        }
    }
    for node in structure.nodes.iter_mut().filter(|node| node.kind == NodeKind::Footnote && node.source == Derivation::Native) {
        if let Some(calls) = node.anchor.as_deref().filter(|anchor| anchor_counts.get(*anchor) == Some(&1)).and_then(|anchor| references.remove(anchor)) {
            node.calls = calls;
        }
    }
    supplement_courtlistener_bookmarked_notes(structure, &text_index, &sibling_note_markers);
    supplement_courtlistener_declared_note_references(structure, &text_index, matched_role_starts);
    clip_courtlistener_pre_note_bodies(structure, &text_index);
    crate::inference::supplement_source_formatted_headings(
        structure,
        heading_exclusions,
        body_exclusions,
    );
    supplement_courtlistener_printed_notes(structure, printed_notes);
    drop_note_numbered_paragraphs(structure);
    courtlistener_division_headings(structure, &text_index);
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
    rendered_blocks: &mut Vec<StructureNode>,
    rendered_exclusions: &mut Vec<ScalarRange>,
    mut recovered_body_blocks: Vec<StructureNode>,
    matched_role_starts: &HashSet<usize>,
) -> (Vec<(StructureNode, Vec<ScalarRange>)>, HashMap<usize, (String, ScalarRange)>) {
    let coordinates = ScalarText::new(text);
    if matches!(scope.kind, ScopeKind::Complete)
        && matches!(
            scope.markup_serialization,
            MarkupSerialization::HtmlSource | MarkupSerialization::BrowserHtml
        )
    {
        if let Some(markup) = markup {
            let notes =
                courtlistener_uncontained_note_blocks(markup, elements, &coordinates, rendered_blocks);
            for note in &notes {
                if let Some(marker) = note.marker_range {
                    rendered_exclusions.push(ScalarRange {
                        start: marker.start,
                        end: note.range.end,
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
            if block.kind == NodeKind::Prose
                && block.markup_start.is_some_and(|start| {
                    matched_role_starts.contains(&start)
                        && element_at(elements, start)
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
                block.kind = NodeKind::Heading;
            }
        }
        let declared = courtlistener_declared_blocks(
            elements,
            rendered_blocks,
            &recovered_body_blocks,
            matched_role_starts,
            &coordinates,
        );
        rendered_blocks.extend(declared);
        recovered_body_blocks.sort_by_key(|block| block.markup_start);
        rendered_blocks.extend(recovered_body_blocks);
    }
    let numbered_note_markers = courtlistener_numbered_note_markers(elements, matched_role_starts, &coordinates);
    let sibling_note_markers = courtlistener_sibling_note_markers(elements, &coordinates);
    for block in rendered_blocks.iter_mut() {
        if block.kind == NodeKind::Footnote {
            if let Some((label, aliases, marker)) = block
                .markup_start
                .and_then(|start| numbered_note_markers.get(&start))
            {
                if block.label.is_none() || block.label == *label {
                    block.label = label.clone();
                    block.aliases = aliases.clone();
                    block.marker_range = Some(*marker);
                    block.range.start = marker.end;
                }
            }
            // (A sibling's number marks the note too where nothing else marked it.)
            if let Some((label, marker)) = block
                .markup_start
                .and_then(|start| sibling_note_markers.get(&start))
            {
                block.label = Some(label.clone());
                block.marker_range.get_or_insert(*marker);
            }
        }
    }
    let printed_notes = match markup.filter(|_| matches!(scope.kind, ScopeKind::Complete)) {
        Some(markup) => courtlistener_printed_note_blocks(markup, elements, &coordinates, rendered_blocks),
        None => Vec::new(),
    };
    (printed_notes, sibling_note_markers)
}

/// A note's label: its marker, or an empty range at its body's start where it prints none.
fn note_label(note: &crate::StructureNode) -> ScalarRange {
    note.marker_range.unwrap_or(ScalarRange { start: note.range.start, end: note.range.start })
}
