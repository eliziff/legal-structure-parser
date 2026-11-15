//! CourtListener's own renditions (html_with_citations, CAP's xml_harvard, Columbia, Lawbox, Lexis,
//! Westlaw, OSCN, public.resource.org): what their markup and printed text supply beyond the generic reading.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;
use legal_structure_model::{ScalarRange, ScopeKind};
use legal_structure_model::document::{DocumentStructure, MarkupElement, NodeKind, StructureNode};
use legal_structure_model::text::{utf16_len, ScalarText};
use regex::Regex;
use crate::text::trim_javascript_whitespace as trim_js;
use super::{
    ancestors, courtlistener_ldml_note, decode_entities, element_at, element_node,
    footnote_identity, javascript_whitespace, leading_footnote_marker, markup_tag_end, parse_tag, short_number,
    MarkupSerialization, NativeMarkupScope,
};
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

/// A CourtListener rendition's reading after derivation: notes paired with their references, and every pass over what the rendition supplies.
pub(super) fn finish(
    structure: &mut DocumentStructure,
    note_references: Vec<(String, ScalarRange)>,
    matched_role_starts: &HashSet<usize>,
    printed_notes: Vec<(StructureNode, Vec<ScalarRange>)>,
) {
    // The text is fixed through these passes: it is indexed once for all of them.
    let text_index = ScalarCoordinates::new(structure.query_text());
    clip_courtlistener_paragraphs_to_opinions(structure, matched_role_starts);
    native_table_coordinates(structure, &text_index);
    // Each link to an anchor (a link's `#target`, CAP's `anchoridref`) calls the note that anchor names, or the note that holds
    // the element it names (Word's `<a href="#_ftn1">` its note's bookmark), from outside that note. The links no note takes
    // are left to name notes only a bookmark marks (`bookmarked_notes`).
    let (calls, references) = {
        let coordinates = ScalarText::with_coordinates(structure.query_text(), &text_index);
        let elements = &structure.markup_elements;
        let idrefs = elements.iter().filter(|element| element.is("footnotereference")).filter_map(|element| {
            Some((element.attributes.get("anchoridref")?.clone(), printed(&coordinates, element.range).filter(|(label, _)| !label.is_empty())?.1))
        }).collect::<Vec<_>>();
        let named = named_elements(elements);
        let mut anchors = HashMap::<&str, Option<usize>>::new();
        for (index, node) in structure.nodes.iter().enumerate() {
            if let Some(anchor) = &node.anchor {
                unique(&mut anchors, anchor.as_str(), Some(index));
            }
        }
        let notes = structure.nodes.iter().enumerate().filter(|(_, node)| node.kind == NodeKind::Footnote).filter_map(|(index, node)| Some((node.markup_start?, index))).collect::<HashMap<_, _>>();
        let mut calls = Vec::new();
        let mut left = HashMap::<String, Vec<ScalarRange>>::new();
        for (target, range) in note_references.into_iter().chain(idrefs) {
            let note = anchors.get(target.as_str()).copied().flatten().filter(|&index| structure.nodes[index].kind == NodeKind::Footnote).or_else(|| {
                let at = named.get(target.as_str()).copied().flatten()?;
                std::iter::once(at).chain(ancestors(elements, at)).find_map(|at| notes.get(&elements[at].markup_start).copied())
            }).filter(|&index| !(structure.nodes[index].range.start <= range.start && range.end <= structure.nodes[index].range.end));
            match note {
                Some(index) => calls.push((index, range)),
                None => left.entry(target).or_default().push(range),
            }
        }
        (calls, left)
    };
    for (index, range) in calls {
        structure.nodes[index].calls.push(range);
    }
    clip_courtlistener_pre_note_bodies(structure, &text_index);
    supplement_courtlistener_printed_notes(structure, printed_notes);
    read_notes(structure, &text_index, references);
    drop_note_numbered_paragraphs(structure);
    courtlistener_source_roles(structure);
    super::printed_pages::courtlistener_pages(structure, &text_index);
    super::titles::shape_headings(structure, &text_index);
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
) -> Vec<(StructureNode, Vec<ScalarRange>)> {
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
    match markup.filter(|_| matches!(scope.kind, ScopeKind::Complete)) {
        Some(markup) => courtlistener_printed_note_blocks(markup, elements, &coordinates, rendered_blocks),
        None => Vec::new(),
    }
}

/// A note's label: its marker, or an empty range at its body's start where it prints none.
fn note_label(note: &crate::StructureNode) -> ScalarRange {
    note.marker_range.unwrap_or(ScalarRange { start: note.range.start, end: note.range.start })
}

/// Each note read once, as it prints and as it is called, after the anchors and bookmarks are paired. Its label is as it
/// prints (`note_opening`): a symbol's slug read as its symbol, a label the source gives none read off its opening, an opening
/// that merely repeats the label set apart as its marker. A number past the next that opens on a reporter's abbreviation,
/// `<a class="footnote">830</a><p>F.Supp. at 1081`, is a citation's volume the source took for a note's number: no note,
/// the end of the one before. A body running on through the next numbers the marks call is that many notes
/// (`listed_notes`). Notes only a bookmark marks are read (`bookmarked_notes`), and notes set flat in the text under their
/// own labels (`flat_notes`). Every other mark (`note_marks`) that no note holds, and that names no note, may name the
/// opinion's own numbered paragraphs set as notes (`notes_set_as_text`); then each finds its note (`answer_calls`), and the
/// marks no note answers stand as `unmatched-call` spans.
fn read_notes(structure: &mut DocumentStructure, text_index: &ScalarCoordinates, links: HashMap<String, Vec<ScalarRange>>) {
    let (notes, mut rest): (Vec<_>, Vec<_>) = std::mem::take(&mut structure.nodes).into_iter().partition(|node| node.kind == NodeKind::Footnote);
    let text = structure.query_text();
    let coordinates = ScalarText::with_coordinates(text, text_index);
    let elements = &structure.markup_elements;
    let marks = note_marks(elements, &coordinates);
    let called = marks.iter().filter_map(|(label, _)| label.parse::<u32>().ok()).collect::<HashSet<_>>();
    let mut read = Vec::<StructureNode>::with_capacity(notes.len());
    // A note no longer read, and the node its children now stand under: the note it joined, or the one over a note set
    // flat notes took the place of.
    let mut joined = HashMap::<String, Option<String>>::new();
    for mut note in notes {
        if note.ranges.is_empty() {
            note.ranges = vec![note.range];
        }
        let (label, echo) = note_opening(coordinates.slice_utf16(note.range.start..note.range.end).unwrap_or(""), note.label.take().unwrap_or_default());
        if echo > 0 {
            let start = note.range.start;
            note.marker_range.get_or_insert(ScalarRange { start, end: start + echo });
            note.range.start = (start + echo).min(note.range.end);
            note.ranges[0].start = (note.ranges[0].start + echo).min(note.ranges[0].end);
        }
        let before = read.iter().rev().find_map(|held| held.label.as_deref().unwrap_or("").parse::<u32>().ok());
        let volume = label.parse::<u32>().ok().zip(before).is_some_and(|(number, before)| number > before + 1)
            && coordinates.slice_utf16(note.range.start..note.range.end).is_some_and(opens_on_reporter);
        if let Some(last) = read.last_mut().filter(|_| volume) {
            last.range.end = note.range.end;
            last.ranges.extend(note.ranges);
            joined.insert(note.id, Some(last.id.clone()));
            continue;
        }
        note.label = Some(label);
        let listed = listed_notes(&mut note, &coordinates, &called);
        read.push(note);
        read.extend(listed);
    }
    bookmarked_notes(elements, &coordinates, utf16_len(text), &links, &mut read);
    flat_notes(text, &coordinates, elements.is_empty(), &mut read, &mut joined);
    for node in rest.iter_mut().chain(read.iter_mut()) {
        if let Some(kept) = node.parent_id.as_ref().and_then(|parent| joined.get(parent)) {
            node.parent_id = kept.clone();
        }
    }
    let mut answered = read.iter().flat_map(|note| note.calls.iter().chain(&note.ranges).copied()).collect::<Vec<_>>();
    answered.sort_by_key(|known| known.end);
    let calls = marks.into_iter().filter(|(_, range)| {
        !answered[answered.partition_point(|known| known.end <= range.start)..].iter().take_while(|known| known.start < range.end || known.end <= range.end).any(|known| known.start < range.end)
    }).map(|(label, range)| {
        let mut call = StructureNode::new(format!("unmatched-call-{}", range.start), NodeKind::Span, range, crate::ENGINE_ORIGIN, Derivation::Native, None);
        call.grammar = Some("unmatched-call".to_owned());
        call.label = Some(label);
        call
    }).collect::<Vec<_>>();
    let opens = rest.iter().filter(|block| matches!(block.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Quote | NodeKind::ListItem))
        .flat_map(|block| [Some(block.range.start), block.marker_range.map(|marker| marker.start)]).flatten().collect::<HashSet<_>>();
    rest.extend(read);
    structure.nodes = rest;
    let set_as_text = notes_set_as_text(structure, text_index, &calls);
    let (mut read, mut rest): (Vec<_>, Vec<_>) = std::mem::take(&mut structure.nodes).into_iter().partition(|node| node.kind == NodeKind::Footnote);
    read.extend(set_as_text);
    read.sort_by_key(|note| note.range.start);
    let text = structure.query_text();
    let calls = answer_calls(text, &ScalarText::with_coordinates(text, text_index), &opens, &mut read, calls);
    rest.extend(read);
    rest.extend(calls);
    structure.nodes = rest;
}

/// Notes set flat in the text, each under its own "Footnote 1:" or "[Note 1]" (the calls that name them, "[FN1]", are
/// `printed_calls`'). A note set flat ends with its own paragraph, ahead of the next note or not: the opinion goes on
/// below it. A `plain` text's lines wrap, so its paragraphs end at a blank line; a paragraph's own mark ends one anywhere.
/// Under the "Footnotes" line a slip opinion sets its notes as a list of their own: each runs to the next. A note the
/// source set round all of them gives way to them, its children standing where it stood (`joined`).
fn flat_notes(text: &str, coordinates: &ScalarText, plain: bool, notes: &mut Vec<StructureNode>, joined: &mut HashMap<String, Option<String>>) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    static PAST: OnceLock<Regex> = OnceLock::new();
    static BROKEN: OnceLock<Regex> = OnceLock::new();
    static LIST: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:(?:Footnote|FN) ?(\d{1,3}|\*+)(?: ?[:.]|[ \t]+\p{Lu}|[ \t]*$)|\[(?:Note|FN) ?(\d{1,3})\])").unwrap());
    let opened = label.captures_iter(text).filter_map(|capture| {
        let whole = capture.get(0)?;
        Some((whole.start(), whole.end(), capture.get(1).or_else(|| capture.get(2))?.as_str().to_owned()))
    }).collect::<Vec<_>>();
    if opened.is_empty() {
        return;
    }
    let closes = if plain { PAST.get_or_init(|| Regex::new(r"\n[ \t]*\n|\n[ \t]*[\[{(]?¶").unwrap()) } else { BROKEN.get_or_init(|| Regex::new(r"\n").unwrap()) };
    let listed = LIST.get_or_init(|| Regex::new(r"(?m)^[ \t]*Footnotes[ \t]*$").unwrap()).find(text).map(|head| head.end());
    for (index, (open, words, number)) in opened.iter().enumerate() {
        let limit = opened.get(index + 1).map_or(text.len(), |next| next.0);
        let body = &text[*words..limit];
        let lead = body.len() - body.trim_start().len();
        let stop = if listed.is_some_and(|head| head <= *open) { words + body.trim_end_matches(javascript_whitespace).len() }
            else { closes.find(&body[lead..]).map_or(limit, |close| words + lead + close.start()) };
        let (start, stop) = (coordinates.utf16_of_byte(*words), coordinates.utf16_of_byte(stop));
        if notes.iter().any(|note| note.label.as_deref().unwrap_or("") == number && note.range.start <= start && start < note.range.end) {
            continue;
        }
        notes.retain(|note| {
            let round = note.range.start <= start && start < note.range.end;
            if round {
                joined.insert(note.id.clone(), note.parent_id.clone());
            }
            !round
        });
        notes.push(found_note(number.clone(), ScalarRange { start, end: stop }, Vec::new(), "flat"));
    }
    notes.sort_by_key(|note| note.range.start);
}

/// The opinion's own numbered blocks that are notes set as text, where no note answers the `calls` the marks leave: the
/// blocks past the last call that print the numbers called, or where a rendition has no note and titles a list of them
/// ("Notes", "Footnotes", "Escolios"), every block under that title that prints a number. A list of notes stands last, and a list in
/// the opinion may print its numbers first, "(3) Was the conduct ...": read from the tail back, each note is the last block
/// that prints its number. The blocks under a note belong to it only where another note closes them, or a title heads the
/// list: past the last note of no list is the opinion, a number having opened a paragraph of it. A paragraph's own mark
/// ends any note. Half the numbers wanted at least, or none.
fn notes_set_as_text(structure: &DocumentStructure, text_index: &ScalarCoordinates, calls: &[StructureNode]) -> Vec<StructureNode> {
    static OPENING: OnceLock<Regex> = OnceLock::new();
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let text = structure.query_text();
    let coordinates = ScalarText::with_coordinates(text, text_index);
    let notes = structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote).collect::<Vec<_>>();
    let labels = notes.iter().map(|note| plain_label(note.label.as_deref().unwrap_or(""))).collect::<HashSet<_>>();
    let unanswered = calls.iter().filter(|call| !labels.contains(called(call))).collect::<Vec<_>>();
    let mut wanted = unanswered.iter().filter_map(|call| called(call).parse::<u32>().ok()).collect::<Vec<_>>();
    wanted.sort_unstable();
    wanted.dedup();
    // A note the source made of a list's title is no note.
    let untitled = notes.iter().all(|note| super::titles::notes_title(coordinates.slice_utf16(note.range.start..note.range.end).map_or("", trim_js)));
    let title = TITLE.get_or_init(|| Regex::new(r"(?mi)^[ \t]*(?:(?:foot|end)?notes?|escolios?)[ \t]*:?[ \t]*$").unwrap()).find_iter(text).last().filter(|_| untitled);
    if wanted.is_empty() && title.is_none() {
        return Vec::new();
    }
    let blocks = super::titles::body_blocks(structure, text_index);
    // A note list's own markers open their blocks: the last call is the last in the text's run.
    let opens = |call: &&StructureNode| blocks.binary_search_by_key(&call.range.start, |block| block.range.start).is_ok();
    let mut last_call = unanswered.iter().filter(|call| !opens(call)).map(|call| call.range.end).max().unwrap_or(0);
    if let Some(title) = title {
        last_call = coordinates.utf16_of_byte(title.end());
        if wanted.is_empty() {
            wanted = (1..1000).collect();
        }
    }
    let opening = OPENING.get_or_init(|| Regex::new(r"^[\[(]{0,2}(\d{1,3})[\])\.]{0,2}(?:\s|\p{Lu})").unwrap());
    let printed_of = |range: ScalarRange| coordinates.slice_utf16(range.start..range.end).map(trim_js);
    let mut chosen = HashMap::new();
    let mut ceiling = u32::MAX;
    for block in blocks.iter().rev().take_while(|block| block.range.start >= last_call) {
        let number = printed_of(block.range).and_then(|printed| opening.captures(printed)).and_then(|capture| capture[1].parse::<u32>().ok());
        if let Some(number) = number.filter(|number| *number < ceiling && wanted.contains(number)) {
            chosen.insert(block.range.start, number);
            ceiling = number;
        }
    }
    let mut found = Vec::<(u32, usize, usize)>::new();
    let mut under = None;
    let mut closed = false;
    for block in blocks.iter().filter(|block| block.range.start >= last_call) {
        let Some(printed) = printed_of(block.range) else { continue };
        if let Some(number) = chosen.get(&block.range.start) {
            if let (Some(last), Some(end)) = (found.last_mut(), under.take()) {
                last.2 = end;
            }
            found.push((*number, block.range.start, block.range.end));
            closed = false;
        } else if super::titles::says_paragraph(printed) {
            under = None;
            closed = true;
        } else if !found.is_empty() && !closed {
            under = Some(block.range.end);
        }
    }
    if let (Some(_), Some(last), Some(end)) = (title, found.last_mut(), under) {
        last.2 = end;
    }
    if found.len() * 2 < wanted.len().min(found.len() + 1) {
        return Vec::new();
    }
    found.into_iter().map(|(number, start, end)| found_note(number.to_string(), ScalarRange { start, end }, Vec::new(), "paragraphs")).collect()
}

/// A note the markup marks and no block declares, its body one piece.
fn found_note(label: String, body: ScalarRange, calls: Vec<ScalarRange>, grammar: &str) -> StructureNode {
    let mut note = super::markup_node(NodeKind::Footnote, label, None);
    note.id = format!("footnote-{grammar}-{}", body.start);
    note.grammar = Some(grammar.to_owned());
    note.range = body;
    note.ranges = vec![body];
    note.calls = calls;
    note
}

/// The notes a body set as one block holds past its own, "... 110. Id. 111. See note 86 supra.": numbers climbing by ones
/// from the note's own, each after a finished sentence and `called` by a mark. Two at least; the note ends where they begin.
fn listed_notes(note: &mut StructureNode, coordinates: &ScalarText, called: &HashSet<u32>) -> Vec<StructureNode> {
    const LEAST_RUN: usize = 2;
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    let number = NUMBER.get_or_init(|| Regex::new(r"(?-u:\b)(\d{1,3})\. +\p{Lu}").unwrap());
    let Ok(mut next) = note.label.as_deref().unwrap_or("").parse::<u32>().map(|label| label + 1) else { return Vec::new() };
    let [range] = note.ranges[..] else { return Vec::new() };
    let Some(body) = coordinates.slice_utf16(range.start..range.end) else { return Vec::new() };
    let mut opens = Vec::new();
    for capture in number.captures_iter(body) {
        let whole = capture.get(0).unwrap();
        if called.contains(&next) && capture[1].parse::<u32>().is_ok_and(|found| found == next) && crate::inference::ends_sentence(body[..whole.start()].trim_end(), true) {
            opens.push((next, whole.start(), capture.get(1).unwrap().end() + 1));
            next += 1;
        }
    }
    if opens.len() < LEAST_RUN {
        return Vec::new();
    }
    let at = |byte: usize| range.start + utf16_len(&body[..byte]);
    note.range.end = at(opens[0].1);
    note.ranges[0].end = note.range.end;
    opens.iter().enumerate().map(|(index, (label, _, words))| {
        let stop = opens.get(index + 1).map_or(range.end, |next| at(next.1));
        found_note(label.to_string(), ScalarRange { start: at(*words), end: stop }, Vec::new(), "list")
    }).collect()
}

/// Notes only a bookmark marks: a number the links no note's anchor took name, from earlier in the text (`<a name="N_1_">1.</a>`
/// and `<a href="#N_1_">`, Word's `<a name="_ftn1" href="#_ftnref1">[1]</a>`). Each runs to the next, within its own block;
/// a word processor's note (`p.MsoFootnoteText`) runs on through the paragraphs of its style that open on no label.
fn bookmarked_notes(elements: &[MarkupElement], coordinates: &ScalarText, end: usize, links: &HashMap<String, Vec<ScalarRange>>, notes: &mut Vec<StructureNode>) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    static OPENS: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^\[?(?:Footnote |FN ?)?(\d{1,3}|\*+|†|‡)\]?[.:)]?$").unwrap());
    let opens = OPENS.get_or_init(|| Regex::new(r"^\[?(\d{1,3}|\*+)\]?[.)]?\s+\S").unwrap());
    let styled = elements.iter().enumerate().filter(|(_, element)| element.is("p") && element.has_class("MsoFootnoteText")).map(|(index, _)| index).collect::<Vec<_>>();
    let block_end = |parent: usize| match styled.binary_search(&parent) {
        Ok(at) => styled[at + 1..].iter().take_while(|&&next| {
            coordinates.slice_utf16(elements[next].range.start..elements[next].range.end).map(trim_js).is_some_and(|printed| !opens.is_match(printed))
        }).last().map_or(elements[parent].range.end, |&last| elements[last].range.end),
        Err(_) => elements[parent].range.end,
    };
    let taken = |range: ScalarRange, notes: &[StructureNode]| notes.iter().any(|note| {
        note.ranges.iter().chain(&note.calls).any(|known| known.start < range.end.max(range.start + 1) && range.start < known.end)
    });
    let mut targets = elements.iter().filter(|element| element.tag == "a").filter_map(|element| {
        let name = element.attributes.get("name").or_else(|| element.attributes.get("id"))?;
        let calls = links.get(name)?;
        let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(trim_js)?;
        let number = label.captures(printed)?[1].to_owned();
        let block = element.parent.map_or(end, block_end);
        (calls.iter().any(|call| call.start < element.range.start) && !taken(element.range, notes)).then(|| (element.range, number, calls.clone(), block))
    }).collect::<Vec<_>>();
    targets.sort_by_key(|target| target.0.start);
    let stops = targets.iter().map(|target| target.0.start).collect::<Vec<_>>();
    for (index, (range, number, calls, block)) in targets.into_iter().enumerate() {
        let stop = stops.get(index + 1).copied().unwrap_or(end).min(block);
        let start = range.end.min(stop);
        notes.push(found_note(number, ScalarRange { start, end: stop }, calls, "bookmarked"));
    }
    notes.sort_by_key(|note| note.range.start);
}

/// The note marks the markup prints, each with the label it names: Harvard's footnotemark, CAP's footnote_reference
/// ("[fn3]") and footnotereference, a superscript, a footnote link. A power (10<sup>8</sup>) and a citation's volume
/// (`<a class="footnote">830</a> F.Supp.`) are none; Colorado's mark is an empty superscript after its digits.
fn note_marks(elements: &[MarkupElement], coordinates: &ScalarText) -> Vec<(String, ScalarRange)> {
    static SYMBOL: OnceLock<Regex> = OnceLock::new();
    let symbol = SYMBOL.get_or_init(|| Regex::new(r"^\[?(?:fn)?(\d{1,3}|\*+|†|‡|\([a-z0-9]\))\]?$").unwrap());
    elements.iter().filter_map(|element| {
        let linked = element.tag == "a" && element.attributes.get("class").is_some_and(|class| class == "footnote")
            && !element.attributes.get("href").is_some_and(|href| href.ends_with("_ref"));
        if !(linked || matches!(element.tag.as_str(), "footnotemark" | "footnote_reference" | "sup") || element.is("footnotereference")) {
            return None;
        }
        let before = (0..4).find_map(|short| coordinates.slice_utf16(element.range.start.saturating_sub(4 - short)..element.range.start)).unwrap_or("");
        let trailing = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let empty = element.range.start == element.range.end && element.attributes.get("class").is_some_and(|class| class == "ldml-superscript");
        if trailing > 0 && !empty && element.tag == "sup" {
            return None;
        }
        let range = if empty { ScalarRange { start: element.range.start - trailing, end: element.range.end } } else { element.range };
        let printed = coordinates.slice_utf16(range.start..range.end).map(trim_js)?;
        let label = symbol.captures(printed)?[1].to_owned();
        let unlinked = element.tag == "a" && !element.attributes.contains_key("href");
        if unlinked && (0..12).find_map(|short| coordinates.slice_utf16(range.end..range.end + 12 - short)).is_some_and(opens_on_reporter) {
            return None;
        }
        Some((label, range))
    }).collect()
}

/// The label a call names, bare.
fn called(call: &StructureNode) -> &str {
    plain_label(call.label.as_deref().unwrap_or(""))
}

/// Which note each of the `calls` answers (`answer_by_label`); a numbered note still uncalled takes a call printed as words
/// (`printed_calls`). The calls no note answers are returned, a mark taken as a printed call among the answered. The
/// opinion answers so again once other texts' notes join.
pub(crate) fn answer_calls(text: &str, coordinates: &ScalarText, opens: &HashSet<usize>, notes: &mut [StructureNode], calls: Vec<StructureNode>) -> Vec<StructureNode> {
    let calls = answer_by_label(notes, calls);
    printed_calls(text, coordinates, opens, notes);
    let mut answered = notes.iter().flat_map(|note| note.calls.iter().copied()).collect::<Vec<_>>();
    answered.sort_by_key(|known| known.start);
    calls.into_iter().filter(|call| answered[..answered.partition_point(|known| known.start < call.range.end)].last().is_none_or(|known| known.end <= call.range.start)).collect()
}

/// Which note a mark answers, by label. The one note of a label takes every mark naming it. A label's several notes (each
/// opinion's own series, or another rendition's copy) share their calls and its marks in the text's order, one apiece, a call
/// found twice a unit or two apart being one; where the marks outnumber them, each note with none takes the next and the rest
/// join the first. The marks no note's label names are returned.
fn answer_by_label(notes: &mut [StructureNode], mut calls: Vec<StructureNode>) -> Vec<StructureNode> {
    calls.sort_by_key(|call| call.range.start);
    let mut by_label = HashMap::<String, Vec<usize>>::new();
    for (index, note) in notes.iter().enumerate() {
        by_label.entry(plain_label(note.label.as_deref().unwrap_or("")).to_owned()).or_default().push(index);
    }
    let (named, left): (Vec<_>, Vec<_>) = calls.into_iter().partition(|call| by_label.contains_key(called(call)));
    let mut marks = HashMap::<String, Vec<ScalarRange>>::new();
    for call in named {
        marks.entry(called(&call).to_owned()).or_default().push(call.range);
    }
    for (label, mut indices) in by_label {
        indices.sort_by_key(|&index| notes[index].range.start);
        let marks = marks.remove(&label).unwrap_or_default();
        if let [only] = indices[..] {
            notes[only].calls.extend(marks);
            continue;
        }
        let mut shared = indices.iter().flat_map(|&index| notes[index].calls.iter().copied()).chain(marks.iter().copied()).collect::<Vec<_>>();
        shared.sort_by_key(|call| call.start);
        shared.dedup_by(|later, kept| later.start <= kept.end + 2);
        if shared.len() <= indices.len() {
            for (position, &index) in indices.iter().enumerate() {
                notes[index].calls = shared.get(position).copied().into_iter().collect();
            }
            continue;
        }
        let mut marks = marks.into_iter();
        for &index in &indices {
            if notes[index].calls.is_empty() {
                notes[index].calls.extend(marks.next());
            }
        }
        notes[indices[0]].calls.extend(marks);
    }
    left
}

/// A numbered note with no call takes a call the text prints as words, "[3]", "[FN3]", "[Note 3]": not one that `opens` a
/// block (a paragraph's number), stands in a note, or opens one (its own label).
fn printed_calls(text: &str, coordinates: &ScalarText, opens: &HashSet<usize>, notes: &mut [StructureNode]) {
    static CALL: OnceLock<Regex> = OnceLock::new();
    if !notes.iter().any(|note| note.calls.is_empty() && note.label.as_deref().unwrap_or("").bytes().all(|byte| byte.is_ascii_digit())) {
        return;
    }
    let mut printed = HashMap::<String, Vec<ScalarRange>>::new();
    let call = CALL.get_or_init(|| Regex::new(r"\[(?:FN ?|fn\.? ?|Note )?(\d{1,3})\]").unwrap());
    for capture in call.captures_iter(text) {
        let whole = capture.get(0).unwrap();
        let unit = coordinates.utf16_of_byte(whole.start());
        let end = unit + utf16_len(whole.as_str());
        let in_note = notes.iter().any(|note| (end..=end + 2).contains(&note.range.start) || note.ranges.iter().any(|range| range.start <= unit && unit < range.end));
        if !opens.contains(&unit) && !in_note {
            printed.entry(capture[1].to_owned()).or_default().push(ScalarRange { start: unit, end });
        }
    }
    let used = notes.iter().flat_map(|note| note.calls.iter().map(|call| call.start)).collect::<HashSet<_>>();
    for note in notes.iter_mut().filter(|note| note.calls.is_empty()) {
        if let Some(calls) = printed.get_mut(note.label.as_deref().unwrap_or("")) {
            calls.retain(|call| !used.contains(&call.start));
            if !calls.is_empty() {
                note.calls.push(calls.remove(0));
            }
        }
    }
}

/// A label as called and as printed compare bare: "(1)", "[1]" and "1" are one note's.
pub(crate) fn plain_label(label: &str) -> &str {
    label.trim_matches(['(', ')', '[', ']', '.', ','])
}

/// Words that open on a reporter's abbreviation: "F.Supp. at 843", "A.2d 12", "O.S.2021".
pub(crate) fn opens_on_reporter(words: &str) -> bool {
    static REPORTER: OnceLock<Regex> = OnceLock::new();
    REPORTER.get_or_init(|| Regex::new(r"^\p{Lu}\p{L}*\.\S").unwrap()).is_match(words.trim_start())
}

/// A note's label where the source gives none ("", or a symbol's slug), and how much of its opening merely repeats it:
/// Harvard's ". The court", Lawbox's "1\n. The court", "[1]  The court".
pub(crate) fn note_opening(body: &str, label: String) -> (String, usize) {
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let opening = OPENING.get_or_init(|| Regex::new(r"^\s*(?:[\[(]?(\d{1,3}[a-zA-Z]?|[a-z]|\*+|†|‡|§|[①-⑳])[\])]?\s*[./]?|\.)\s+").unwrap());
    // CourtListener's slug for a symbol: "-s" a star, "-s-s" two, "-s--2" a star's second use.
    let label = label.trim().to_owned();
    let label = if label.starts_with("-s") { "*".repeat(label.split("--").next().unwrap_or("").matches("-s").count()) } else { label };
    let label = if plain_label(&label).starts_with(['*', '†', '‡']) { plain_label(&label).to_owned() } else { label };
    let unnamed = label.is_empty() || label.starts_with('-') || label == "None";
    let Some(capture) = opening.captures(body) else { return (if unnamed { String::new() } else { label }, 0) };
    let printed = capture.get(1).map(|printed| printed.as_str());
    // "330 F.Supp. at 843", "43 O.S.2021 § 109.3": a note that opens on a citation, its volume no label.
    if unnamed && !capture[0].contains(['.', ')', ']']) && opens_on_reporter(&body[capture[0].len()..]) {
        return (String::new(), 0);
    }
    match printed {
        // A lone letter opens a sentence as readily as a note ("A court ..."), so it is named only if the label says so.
        Some(printed) if unnamed && !printed.chars().all(char::is_alphabetic) => (printed.to_owned(), utf16_len(&capture[0])),
        Some(_) if unnamed => (String::new(), 0),
        Some(printed) if printed != label.trim_matches(['(', ')', '[', ']', '.', '/']) => (label, 0),
        _ => (label, utf16_len(&capture[0])),
    }
}
