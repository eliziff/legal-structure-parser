//! A document's own outline: its headings and its top-level sections, in order.

use legal_structure::{
    Derivation, DocumentStructure, NodeKind, PrimaryRole, ScalarText, StructureNode,
};
use crate::PrintedStatute;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineEntry {
    /// "heading" or "section".
    kind: &'static str,
    /// Headings nest by their own hierarchy; a section sits under the heading before it.
    level: usize,
    title: String,
    /// The node's start in the document's query text.
    start: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_index: Option<usize>,
}

/// A section label as a reader cites it: "sec33.1" is "s 33.1", "part2" "Part 2".
fn section_title(label: &str) -> String {
    for (prefix, name) in [
        ("sec", "s "),
        ("part", "Part "),
        ("sched", "Schedule "),
        ("art", "Art "),
        ("ann", "Annex "),
        ("app", "Appendix "),
    ] {
        if let Some(rest) = label.strip_prefix(prefix).filter(|rest| !rest.is_empty()) {
            return format!("{name}{rest}");
        }
    }
    label.to_owned()
}

fn own_start(node: &StructureNode) -> usize {
    node.rendered_membership
        .iter()
        .map(|range| range.start)
        .min()
        .or_else(|| node.rendered_range.map(|range| range.start))
        .or_else(|| node.ranges.iter().map(|range| range.start).min())
        .unwrap_or(node.range.start)
}

fn outline_entries(
    structure: &DocumentStructure,
    sections: bool,
    literal: bool,
) -> Vec<OutlineEntry> {
    let text = ScalarText::new(structure.query_text());
    let source_text = ScalarText::new(&structure.text);
    let nodes = structure
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let depth = |node: &StructureNode, counts: &dyn Fn(&StructureNode) -> bool| {
        let (mut level, mut parent) = (0usize, node.parent_id.as_deref());
        let mut seen = HashSet::new();
        while let Some(found) = parent
            .filter(|id| seen.insert(*id))
            .and_then(|id| nodes.get(id))
        {
            if counts(found) {
                level += 1;
            }
            parent = found.parent_id.as_deref();
        }
        level
    };
    let ldml = |node: &StructureNode, tag: &str, class: &str| {
        node.source == Derivation::Native
            && node.markup_tag.as_deref() == Some(tag)
            && node
                .markup_attributes
                .get("class")
                .is_some_and(|classes| classes.split_ascii_whitespace().any(|value| value == class))
    };
    let mut entries = structure
        .nodes
        .iter()
        .filter_map(|node| {
            if literal && node.role != Some(PrimaryRole::Heading) {
                return None;
            }
            if node.kind == NodeKind::Heading
                && node.role.is_some_and(|role| role != PrimaryRole::Heading)
            {
                return None;
            }
            let kind = if node.role == Some(PrimaryRole::Heading) {
                NodeKind::Heading
            } else {
                node.kind
            };
            let start = own_start(node);
            let page_index = node.page_indexes.first().copied();
            let label = if let Some(range) = node.heading_range {
                source_text.slice_utf16(range.start..range.end)?.to_owned()
            } else if kind == NodeKind::Heading
                && (node.role == Some(PrimaryRole::Heading) || node.source == Derivation::Native)
                && (node.role == Some(PrimaryRole::Heading) || node.markup_tag.is_some())
            {
                node.display_value.clone().unwrap_or_else(|| {
                    if !node.rendered_membership.is_empty() {
                        node.rendered_membership
                            .iter()
                            .filter_map(|range| text.slice_utf16(range.start..range.end))
                            .collect::<Vec<_>>()
                            .join(" ")
                    } else {
                        let membership = if node.ranges.is_empty() {
                            std::slice::from_ref(&node.range)
                        } else {
                            &node.ranges
                        };
                        membership
                            .iter()
                            .filter_map(|range| source_text.slice_utf16(range.start..range.end))
                            .collect::<Vec<_>>()
                            .join(" ")
                    }
                })
            } else {
                node.label.clone()?
            }
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
            let entry = match kind {
                NodeKind::Heading
                    if !label.is_empty()
                        && (node.role == Some(PrimaryRole::Heading)
                            || label.chars().count() <= 200) =>
                {
                    Some(OutlineEntry {
                        kind: "heading",
                        // The heading and nested sections are source siblings,
                        // so their hierarchy comes from the containing LDML sections.
                        level: if literal {
                            node.level.unwrap_or(0)
                        } else if ldml(node, "section", "ldml-heading") {
                            depth(node, &|parent| {
                                parent.kind == NodeKind::Section
                                    && ldml(parent, "div", "ldml-section")
                            })
                            .saturating_sub(1)
                        } else {
                            depth(node, &|parent| {
                                parent.role == Some(PrimaryRole::Heading)
                                    || parent.role.is_none() && parent.kind == NodeKind::Heading
                            })
                        },
                        title: label,
                        start,
                        page_index,
                    })
                }
                NodeKind::Section
                    if sections && depth(node, &|parent| parent.kind == NodeKind::Section) == 0 =>
                {
                    Some(OutlineEntry {
                        kind: "section",
                        level: 0,
                        title: section_title(&label),
                        start,
                        page_index,
                    })
                }
                _ => None,
            }?;
            Some((node, entry))
        })
        .collect::<Vec<_>>();
    // Apply the existing repeated-title view rule before observing sibling
    // ranks: an omitted entry must not set a surviving heading's level.
    let mut counts = HashMap::<String, usize>::new();
    for (_, entry) in entries.iter().filter(|(_, entry)| entry.kind == "heading") {
        *counts.entry(entry.title.to_lowercase()).or_default() += 1;
    }
    entries.retain(|(node, entry)| {
        node.role == Some(PrimaryRole::Heading)
            || entry.kind != "heading"
            || counts[&entry.title.to_lowercase()] < 3
    });
    if literal {
        return entries.into_iter().map(|(_, entry)| entry).collect();
    }
    let mut ranked = entries
        .iter()
        .enumerate()
        .filter_map(|(index, (node, _))| {
            if node.kind != NodeKind::Heading || node.source != Derivation::Native {
                return None;
            }
            let rank = match node.markup_tag.as_deref()?.as_bytes() {
                [b'h', rank @ b'1'..=b'6'] => *rank,
                _ => return None,
            };
            let start = node.markup_start?;
            let element = structure
                .markup_elements
                .binary_search_by_key(&start, |element| element.markup_start)
                .ok()
                .map(|index| &structure.markup_elements[index])?;
            Some((start, index, element.parent, rank))
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable_by_key(|&(start, _, _, _)| start);
    let mut rank_stacks = HashMap::<Option<usize>, Vec<u8>>::new();
    for (_, index, parent, rank) in ranked {
        let stack = rank_stacks.entry(parent).or_default();
        while stack.last().is_some_and(|previous| *previous >= rank) {
            stack.pop();
        }
        // Sibling Hn declarations supply relative depth within their actual
        // source parent; skipped ranks do not create synthetic headings.
        entries[index].1.level += stack.len();
        stack.push(rank);
    }
    entries.into_iter().map(|(_, entry)| entry).collect()
}

/// The document's own outline: its headings and its top-level sections, in order. A PDF's
/// numbered sections come from `printed`, the reading of its body as a statute, given only
/// for legislation, and only where its sections each appear once, in order; a judgment's
/// numbers are not sections.
pub fn document_outline(
    structure: &DocumentStructure,
    pdf: bool,
    printed: Option<&PrintedStatute>,
) -> Vec<OutlineEntry> {
    // Exact PDF source identity and established role and level facts select literal
    // projection. Other sources retain their existing markup view rules.
    let literal = pdf
        && !structure.source_fragments.is_empty()
        && structure.nodes.iter().any(|node| node.role.is_some())
        && structure
            .nodes
            .iter()
            .filter(|node| node.role.is_some())
            .all(|node| node.level.is_some());
    let mut entries = outline_entries(structure, !pdf, literal);
    let explicit_headings = structure
        .nodes
        .iter()
        .filter(|node| !literal && node.role == Some(PrimaryRole::Heading))
        .map(own_start)
        .collect::<HashSet<_>>();
    let declared_entries = entries
        .iter()
        .filter(|entry| entry.kind == "heading" && explicit_headings.contains(&entry.start))
        .map(|entry| {
            (
                entry.start,
                entry.level,
                entry.title.clone(),
                entry.page_index,
            )
        })
        .collect::<Vec<_>>();
    entries.retain(|entry| entry.kind != "heading" || !explicit_headings.contains(&entry.start));
    if let Some(instrument) = printed {
        let sections = outline_entries(instrument.structure(), true, false)
            .into_iter()
            .filter(|entry| entry.kind == "section")
            .collect::<Vec<_>>();
        let number = |title: &str| {
            title
                .strip_prefix("s ")
                .and_then(|rest| rest.split('.').next()?.parse::<u32>().ok())
        };
        let ordered = !sections.is_empty() && sections.windows(2).all(|pair| {
            matches!((number(&pair[0].title), number(&pair[1].title)), (Some(a), Some(b)) if a < b
                || a == b && pair[0].title < pair[1].title)
        });
        if ordered {
            // A section opens where the PDF paragraph it starts in does.
            entries.extend(sections.into_iter().filter_map(|entry| {
                let node =
                    &structure.nodes[instrument.parts(entry.start, entry.start + 1).next()?.0];
                Some(OutlineEntry {
                    start: own_start(node),
                    level: if literal {
                        node.level.unwrap_or(0)
                    } else {
                        entry.level
                    },
                    page_index: node.page_indexes.first().copied(),
                    ..entry
                })
            }));
        }
    }
    entries.sort_by_key(|entry| entry.start);
    if literal {
        return entries;
    }
    // The matter before a judgment's reasons (the style of cause, the court, the parties and
    // counsel, a date) is no heading of the text: the outline opens with the headings that run
    // straight into its first numbered paragraph, as its case outline reads it ("[1]"), with no
    // text between them. An enactment's title pages (its title block, assent, a consolidation's
    // cover) are the pages before its first section.
    let start = |node: &StructureNode| node.rendered_range.unwrap_or(node.range).start;
    // Entries start in the query text: the rendered text where the document has one.
    let text = structure
        .rendered_text
        .as_deref()
        .unwrap_or(&structure.text);
    let paragraph = legal_structure::case_outline(text)
        .ok()
        .and_then(|outline| outline.into_iter().find(|entry| entry.kind == "paragraph"))
        .map(|entry| entry.start);
    if paragraph.is_none() {
        // A judgment's section nodes are a misreading; an enactment's sections are its printed reading's.
        if let Some(page) = entries
            .iter()
            .filter(|entry| entry.kind == "section")
            .filter_map(|entry| entry.page_index)
            .min()
        {
            entries.retain(|entry| {
                entry.kind != "heading" || entry.page_index.is_none_or(|at| at >= page)
            });
        }
    }
    if let Some(body) = paragraph {
        let declared_headings = structure
            .nodes
            .iter()
            .filter(|node| {
                node.kind == NodeKind::Heading
                    && node.source == Derivation::Native
                    && node.markup_tag.is_some()
            })
            .map(start)
            .collect::<HashSet<_>>();
        let mut opens = body;
        for entry in entries
            .iter()
            .rev()
            .filter(|entry| entry.kind == "heading" && entry.start < body)
        {
            // Text lying wholly between this heading and what follows it; the paragraph that
            // opens the body, whatever its parts, is not between.
            let text_between = structure.nodes.iter().any(|node| {
                matches!(
                    node.kind,
                    NodeKind::Prose | NodeKind::List | NodeKind::Table | NodeKind::Paragraph
                ) && start(node) > entry.start
                    && node.rendered_range.unwrap_or(node.range).end <= opens
            });
            // A date alone ("March 31, 2021") dates the reasons; it heads nothing.
            if text_between || !legal_citations::cues::layout_heading_text_plausible(&entry.title) {
                break;
            }
            opens = entry.start;
        }
        // A bracketed footnote can be mistaken for the first body paragraph.
        // That heuristic must not remove headings declared by the source.
        entries.retain(|entry| {
            entry.kind != "heading"
                || entry.start >= opens
                || declared_headings.contains(&entry.start)
        });
    }
    let mut heading = None;
    for entry in &mut entries {
        if entry.kind == "heading" {
            heading = Some(entry.level);
        } else {
            entry.level = heading.map_or(0, |level| level + 1);
        }
    }
    for (start, level, title, page_index) in declared_entries {
        entries.push(OutlineEntry {
            kind: "heading",
            level,
            title,
            start,
            page_index,
        });
    }
    entries.sort_by_key(|entry| entry.start);
    entries
}
