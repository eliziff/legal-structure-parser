//! A document's outline, read straight from its nodes: its headings, provisions, paragraphs, list items, quotations and
//! front matter in order, each at its level under its parents, and the lines no node of them holds.

use legal_structure::{
    DocumentStructure, EngineError, NodeKind, PrimaryRole, ProviderTextSourceKind, ScalarRange, ScalarText, StructureNode,
};
use serde::Serialize;

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OutlineEntry {
    /// The node's kind ("heading", "section", "paragraph", "prose", "list_item", "quote", or "span", a line of the front
    /// matter), or "text": lines no node of the outline holds.
    kind: &'static str,
    /// Its depth under its parents, from 0; a line of text one under the node holding it.
    level: usize,
    /// Its number or enumerator as printed: "[12]", "(2)", "PART XXIII", "IV.".
    #[serde(skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<String>", optional))]
    label: String,
    /// A heading's words; a provision's marginal note.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    title: Option<String>,
    /// Its own words, up to the first entry under it, without the source's emphasis marks: a line to each paragraph.
    #[serde(skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<String>", optional))]
    text: String,
    /// Its span in the document's query text; a provision's opens with its marginal note.
    start: usize,
    end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    role: Option<PrimaryRole>,
    /// A provision's kind as its marker reads it: "section", "part", "schedule", ...
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    locator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    page_index: Option<usize>,
}

/// The outline's name for a node it holds.
fn entry_kind(node: &StructureNode) -> Option<&'static str> {
    if node.role == Some(PrimaryRole::Heading) {
        return Some("heading");
    }
    match node.kind {
        NodeKind::Heading => node.role.is_none().then_some("heading"),
        NodeKind::Span => (node.grammar.as_deref() == Some("front")).then_some("span"),
        NodeKind::Section | NodeKind::Paragraph | NodeKind::Prose | NodeKind::ListItem | NodeKind::Quote => {
            node.role.is_none_or(|role| role == PrimaryRole::Text).then(|| node.kind.name())
        }
        _ => None,
    }
}

/// The words without a provider's emphasis marks, a line to each paragraph.
fn plain(value: &str) -> String {
    value.lines()
        .map(|line| line.replace(['*', '_'], "").trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The document's outline. In a text the structure renders apart from its source (a PDF's), each entry stands at its
/// place in the rendered text and carries no words of its own.
pub fn document_outline(structure: &DocumentStructure) -> Vec<OutlineEntry> {
    let source = ScalarText::new(&structure.text);
    let query = ScalarText::new(structure.query_text());
    let rendered = structure.rendered_text.is_some();
    let words = |range: ScalarRange| source.slice_utf16(range.start..range.end).map(plain).unwrap_or_default();
    let mut nodes = structure.nodes.iter().filter_map(|node| Some((node, entry_kind(node)?))).collect::<Vec<_>>();
    let span = |node: &StructureNode| {
        if rendered {
            return structure.query_range(node);
        }
        let note = node.heading_range.filter(|_| node.kind == NodeKind::Section).map_or(node.range.start, |note| note.start);
        ScalarRange { start: note.min(node.range.start), end: node.range.end }
    };
    nodes.sort_by_key(|(node, _)| (span(node).start, node.level));
    let mut entries = Vec::with_capacity(nodes.len());
    // The lines up to `end` from where the last entry's own words stopped, at the depth of the deepest entry holding
    // them.
    let mut covered = 0;
    let mut holding = Vec::<(usize, usize)>::new();
    let lines = |entries: &mut Vec<OutlineEntry>, holding: &[(usize, usize)], from: usize, end: usize| {
        let Some(stretch) = (from < end).then(|| source.slice_utf16(from..end)).flatten() else { return };
        let mut at = from;
        for line in stretch.split_inclusive('\n') {
            let start = at + legal_structure::utf16_len(&line[..line.len() - line.trim_start().len()]);
            at += legal_structure::utf16_len(line);
            let text = plain(line);
            if text.is_empty() {
                continue;
            }
            let level = holding.iter().rev().find(|(_, holds)| *holds > start).map_or(0, |(level, _)| level + 1);
            entries.push(OutlineEntry { kind: "text", level, label: String::new(), title: None, text, start,
                end: start + legal_structure::utf16_len(line.trim()), role: None, locator: None, page_index: None });
        }
    };
    for (index, &(node, kind)) in nodes.iter().enumerate() {
        let place = span(node);
        let level = node.level.map_or(0, |level| level.saturating_sub(1));
        let heading = kind == "heading";
        let label = match node.marker_range {
            Some(marker) if !rendered => words(marker),
            _ if node.kind == NodeKind::Section && !rendered => node.content_start
                .map(|content| words(ScalarRange { start: node.range.start, end: content }).trim_start_matches('#').trim().to_owned())
                .unwrap_or_default(),
            _ if node.kind == NodeKind::ListItem => node.label.clone().unwrap_or_default(),
            _ => String::new(),
        };
        let within = |range: &ScalarRange| node.range.start <= range.start && range.end <= node.range.end;
        let title = if heading {
            node.display_value.clone()
                .or_else(|| node.heading_range.filter(|_| !rendered).map(words))
                .or_else(|| node.label.clone())
                .or_else(|| rendered.then(|| structure.query_ranges(node).iter()
                    .filter_map(|range| query.slice_utf16(range.start..range.end)).collect::<Vec<_>>().join(" ")))
                .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|title| !title.is_empty())
        } else {
            node.heading_range.filter(|note| !rendered && !within(note)).map(words)
        };
        let mut entry = OutlineEntry { kind, level, label, title, text: String::new(), start: place.start, end: place.end,
            role: node.role.filter(|_| kind == "span"), locator: node.locator_kind.clone(),
            page_index: node.page_indexes.first().copied() };
        if rendered {
            entries.push(entry);
            continue;
        }
        let own = match node.kind {
            _ if heading => node.heading_range.filter(within).map_or(node.range.end, |line| line.end),
            NodeKind::Span | NodeKind::Quote => node.range.start,
            _ => [node.marker_range.map(|marker| marker.end), node.content_start].into_iter().flatten()
                .fold(node.range.start, usize::max),
        };
        // Its own words run to the first entry under it.
        let own_end = nodes.get(index + 1).map(|(next, _)| span(next).start).filter(|&next| next < node.range.end)
            .unwrap_or(node.range.end).max(own);
        lines(&mut entries, &holding, covered, place.start);
        entry.text = words(ScalarRange { start: own, end: own_end });
        entries.push(entry);
        holding.retain(|(_, holds)| *holds > place.start);
        holding.push((level, node.range.end));
        covered = covered.max(own_end);
    }
    if !rendered {
        lines(&mut entries, &holding, covered, source.utf16_len());
    }
    entries
}

/// The outline of a provider's text of a judgment or a statute (`provider_text_outline_reading`).
pub fn text_outline(text: &str, source_kind: ProviderTextSourceKind) -> Result<Vec<OutlineEntry>, EngineError> {
    Ok(document_outline(&legal_structure::provider_text_outline_reading(text, source_kind)?))
}
