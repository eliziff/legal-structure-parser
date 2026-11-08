//! What the parse read, as the structure the product serves: its pages, paragraphs, notes, headings, headnotes and parts are
//! nodes over the reading text, each node's own reading in its `properties`; the rest is the structure's `opinion` reading.
//! `OpinionLayer::of` reads the lists back from a structure, for the tools that ask for them by kind.

use super::*;
use crate::{DetectionProfile, Note, NoteKindV2, NoteReference, Origin, Scope};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

const ORIGIN: &str = "courtlistener-opinion";
const PART: &str = "opinion-part-";

/// A record read whole, as the structure the product serves.
pub fn opinion_structure(record: &OpinionRecord, url: Option<String>) -> DocumentStructure {
    opinion_layer(record).into_structure(url)
}

/// A reading's name for a thing kept as a `&'static str`: the few the parse uses, each kept once.
fn interned(value: &str) -> &'static str {
    static KNOWN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut known = KNOWN.get_or_init(Default::default).lock().unwrap();
    if let Some(name) = known.get(value) {
        return name;
    }
    let name: &'static str = Box::leak(value.to_owned().into_boxed_str());
    known.insert(name);
    name
}

fn set(node: &mut StructureNode, name: &str, value: impl Into<String>) {
    node.properties.insert(name.to_owned(), value.into());
}

fn flag(node: &mut StructureNode, name: &str, on: bool) {
    if on {
        set(node, name, "true");
    }
}

fn property<'a>(node: &'a StructureNode, name: &str) -> Option<&'a str> {
    node.properties.get(name).map(String::as_str)
}

fn on(node: &StructureNode, name: &str) -> bool {
    property(node, name) == Some("true")
}

/// A label's bare number or letters: "[12]" is 12, "(a)." is a.
fn bare(label: &str) -> &str {
    label.trim_matches(['(', ')', '[', ']', '{', '}', '.', ':', '*', ' '])
}

impl OpinionLayer {
    pub(super) fn into_structure(self, url: Option<String>) -> DocumentStructure {
        let derived = if self.rendition == "plain_text.txt" { Derivation::Heuristic } else { Derivation::Native };
        let node = |id: String, kind: NodeKind, start: usize, end: usize, source: Derivation| {
            StructureNode::new(id, kind, ScalarRange { start, end }, ORIGIN, source, None)
        };
        let mut nodes = Vec::new();
        let mut notes = Vec::new();
        let mut pins = BTreeMap::new();
        for (index, part) in self.parts.iter().enumerate() {
            let mut section = node(format!("{PART}{}", index + 1), NodeKind::Section, part.start, part.end, derived);
            section.aliases = Some(vec![part.kind.clone()]);
            set(&mut section, "kind", part.kind.clone());
            if let Some(author) = &part.author {
                set(&mut section, "author", author.clone());
            }
            nodes.push(section);
        }
        let mut note_spans = Vec::new();
        for (index, note) in self.notes.iter().enumerate() {
            let id = format!("opinion-note-{}", index + 1);
            let mut footnote = node(id.clone(), NodeKind::Footnote, note.start, note.end, derived);
            footnote.ranges = note.ranges.clone();
            let label = bare(&note.label);
            footnote.label = (!label.is_empty()).then(|| format!("fn{label}"));
            footnote.aliases = Some(vec![note.label.clone(), format!("footnote {}", note.label)]);
            set(&mut footnote, "label", note.label.clone());
            if !note.items.is_empty() {
                set(&mut footnote, "items", note.items.join("\n"));
            }
            flag(&mut footnote, "called_outside", note.called_outside);
            if let Some(page) = &note.page {
                set(&mut footnote, "page", page.clone());
            }
            if !note.pins.is_empty() {
                pins.insert(id.clone(), note.pins.clone());
            }
            let body = ScalarRange { start: note.start, end: note.end };
            notes.push(Note {
                id: format!("note-{id}"), node_id: Some(id.clone()), kind: NoteKindV2::Footnote,
                label_range: ScalarRange { start: note.start, end: note.start }, label_ranges: Vec::new(),
                body_range: body, body_ranges: note.ranges.clone(),
                primary_reference: note.references.first().copied(),
                references: note.references.iter().map(|range| NoteReference { range: *range, anchor: None, page_indexes: Vec::new(), line_ids: Vec::new() }).collect(),
            });
            note_spans.extend(note.ranges.iter().map(|range| (range.start, range.end, id.clone())));
            nodes.push(footnote);
        }
        for (index, mut page) in self.pages.into_iter().enumerate() {
            page.id = format!("opinion-page-{}", index + 1);
            page.parent_id = page.in_note().then(|| note_spans.iter().find(|(start, end, _)| *start <= page.range.start && page.range.start < *end).map(|(_, _, id)| id.clone())).flatten();
            nodes.push(page);
        }
        for (index, paragraph) in self.paragraphs.iter().enumerate() {
            let id = format!("opinion-paragraph-{}", index + 1);
            let number = paragraph.number.as_deref().map(bare).filter(|number| !number.is_empty());
            let kind = if number.is_some() { NodeKind::Paragraph } else if paragraph.quote { NodeKind::Quote } else { NodeKind::Prose };
            let mut block = node(id.clone(), kind, paragraph.start, paragraph.end, derived);
            block.label = number.map(|number| format!("par{number}"));
            block.parent_id = paragraph.part.map(|part| format!("{PART}{}", part + 1));
            for (name, value) in [("number", &paragraph.number), ("item", &paragraph.item), ("speaker", &paragraph.speaker), ("page", &paragraph.page)] {
                if let Some(value) = value {
                    set(&mut block, name, value.clone());
                }
            }
            flag(&mut block, "quote", paragraph.quote);
            flag(&mut block, "continues", paragraph.continues);
            if !paragraph.pins.is_empty() {
                pins.insert(id, paragraph.pins.clone());
            }
            nodes.push(block);
        }
        let mut ladder = Vec::<(usize, String)>::new();
        for (index, heading) in self.headings.iter().enumerate() {
            let id = format!("opinion-heading-{}", index + 1);
            let mut title = node(id.clone(), NodeKind::Heading, heading.start, heading.end, derived);
            title.heading_range = Some(ScalarRange { start: heading.start, end: heading.end });
            title.display_value = Some(heading.text.clone());
            // Under the last title above it of a higher level.
            ladder.retain(|(level, _)| *level < heading.level);
            title.parent_id = ladder.last().map(|(_, id)| id.clone());
            ladder.push((heading.level, id.clone()));
            set(&mut title, "level", heading.level.to_string());
            set(&mut title, "source", heading.source);
            if let Some(page) = &heading.page {
                set(&mut title, "page", page.clone());
            }
            if !heading.pins.is_empty() {
                pins.insert(id, heading.pins.clone());
            }
            nodes.push(title);
        }
        let mut unplaced = Vec::new();
        for (index, headnote) in self.headnotes.iter().enumerate() {
            let Some(range) = headnote.range else {
                unplaced.push(headnote.clone());
                continue;
            };
            let mut point = node(format!("opinion-headnote-{}", index + 1), NodeKind::Prose, range.start, range.end, derived);
            point.role = Some(PrimaryRole::Headnote);
            point.display_value = Some(headnote.text.clone());
            set(&mut point, "kind", headnote.kind.clone());
            set(&mut point, "index", index.to_string());
            for (name, value) in [("number", &headnote.number), ("topic", &headnote.topic)] {
                if let Some(value) = value {
                    set(&mut point, name, value.clone());
                }
            }
            nodes.push(point);
        }
        nodes.sort_by_key(|node| (node.range.start, std::cmp::Reverse(node.range.end)));
        let census = serde_json::json!({
            "page_witness": self.page_witness, "note_witness": self.note_witness, "heading_witness": self.heading_witness,
            "paragraph_witness": self.paragraph_witness, "paragraphs_run_on": self.paragraphs_run_on, "paragraphs_split": self.paragraphs_split,
            "union_test": self.union_test, "union_samples": self.union_samples, "titles_not_held": self.titles_not_held,
        });
        let reading = OpinionReading {
            rendition: self.rendition, citations: self.citations, page_series: self.page_series, pins, headnotes: unplaced,
            headnote_marks: self.headnote_marks, blocks: self.blocks, front: self.front, furniture: self.furniture,
            unmatched_calls: self.unmatched_calls, tables: self.tables, defined_terms: self.defined_terms, quotes: self.quotes,
            instruments: self.instruments, record_gaps: self.record_gaps, outcomes: self.outcomes, document_kind: self.document_kind,
            other_opinion: self.other_opinion, census,
        };
        let text_sha256 = format!("{:x}", Sha256::digest(self.text.as_bytes()));
        let mut structure = DocumentStructure::from_projected_parts(
            self.id, "courtlistener".to_owned(), url, Some("cases".to_owned()), Some(DetectionProfile::CaseLossy), self.text, text_sha256,
            None, Scope::complete(), vec![Origin { id: ORIGIN.to_owned() }], nodes, notes, Vec::new(),
        );
        structure.cited_authorities = self.cited;
        structure.opinion = Some(Box::new(reading));
        structure.refresh_revision().expect("Typed document structure is serializable");
        structure
    }

    /// The lists read back from a structure the parse wrote, as the parse held them.
    pub fn of(structure: &DocumentStructure) -> Self {
        let reading = structure.opinion.as_deref().cloned().unwrap_or_default();
        let kind = |kind: NodeKind| structure.nodes.iter().filter(move |node| node.kind == kind && node.origin_id == ORIGIN);
        let numbered = |prefix: &str, id: &str| id.strip_prefix(prefix).and_then(|index| index.parse::<usize>().ok()).unwrap_or(0);
        let pins = |id: &str| reading.pins.get(id).cloned().unwrap_or_default();
        let text = |node: &StructureNode, name: &str| property(node, name).map(str::to_owned);
        let mut pages = kind(NodeKind::Page).map(|node| (numbered("opinion-page-", &node.id), node.clone())).collect::<Vec<_>>();
        pages.sort_by_key(|page| page.0);
        let mut paragraphs = structure.nodes.iter().filter(|node| node.origin_id == ORIGIN && node.id.starts_with("opinion-paragraph-")).map(|node| (numbered("opinion-paragraph-", &node.id), OpinionParagraph {
            item: text(node, "item"), continues: on(node, "continues"), number: text(node, "number"), start: node.range.start, end: node.range.end,
            page: text(node, "page"), quote: on(node, "quote"), part: node.parent_id.as_deref().map(|part| numbered(PART, part) - 1),
            speaker: text(node, "speaker"), pins: pins(&node.id),
        })).collect::<Vec<_>>();
        paragraphs.sort_by_key(|paragraph| paragraph.0);
        let mut notes = kind(NodeKind::Footnote).map(|node| {
            let note = structure.notes.iter().find(|note| note.node_id.as_deref() == Some(node.id.as_str()));
            (numbered("opinion-note-", &node.id), OpinionNote {
                items: property(node, "items").map_or_else(Vec::new, |items| items.split('\n').map(str::to_owned).collect()),
                label: text(node, "label").unwrap_or_default(), start: node.range.start, end: node.range.end, ranges: node.ranges.clone(),
                references: note.map_or_else(Vec::new, |note| note.references.iter().map(|reference| reference.range).collect()),
                called_outside: on(node, "called_outside"), page: text(node, "page"), pins: pins(&node.id),
            })
        }).collect::<Vec<_>>();
        notes.sort_by_key(|note| note.0);
        let mut headings = kind(NodeKind::Heading).map(|node| (numbered("opinion-heading-", &node.id), OpinionHeading {
            text: node.display_value.clone().unwrap_or_default(), level: property(node, "level").and_then(|level| level.parse().ok()).unwrap_or(1),
            start: node.range.start, end: node.range.end, page: text(node, "page"), source: interned(property(node, "source").unwrap_or_default()),
            pins: pins(&node.id), line: 0,
        })).collect::<Vec<_>>();
        headings.sort_by_key(|heading| heading.0);
        let mut parts = kind(NodeKind::Section).map(|node| (numbered(PART, &node.id), OpinionPart {
            kind: text(node, "kind").unwrap_or_default(), author: text(node, "author"), start: node.range.start, end: node.range.end,
        })).collect::<Vec<_>>();
        parts.sort_by_key(|part| part.0);
        // The headnotes in their order: those the text carries at their places in it, the cluster's in the gaps.
        let placed = structure.nodes.iter().filter(|node| node.origin_id == ORIGIN && node.role == Some(PrimaryRole::Headnote)).filter_map(|node| {
            Some((property(node, "index")?.parse::<usize>().ok()?, OpinionHeadnote {
                kind: text(node, "kind").unwrap_or_default(), number: text(node, "number"), topic: text(node, "topic"),
                text: node.display_value.clone().unwrap_or_default(), range: Some(node.range),
            }))
        }).collect::<BTreeMap<_, _>>();
        let mut unplaced = reading.headnotes.into_iter();
        let mut headnotes = Vec::new();
        for at in 0..placed.len() + unplaced.len() {
            match placed.get(&at) {
                Some(headnote) => headnotes.push(headnote.clone()),
                None => headnotes.extend(unplaced.next()),
            }
        }
        fn witness<T: serde::de::DeserializeOwned + Default>(census: &serde_json::Value, name: &str) -> T {
            census.get(name).cloned().and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default()
        }
        let census = &reading.census;
        OpinionLayer {
            id: structure.document_id.clone(), rendition: reading.rendition, text: structure.text.clone(),
            pages: pages.into_iter().map(|page| page.1).collect(), paragraphs: paragraphs.into_iter().map(|paragraph| paragraph.1).collect(),
            blocks: reading.blocks, notes: notes.into_iter().map(|note| note.1).collect(), headings: headings.into_iter().map(|heading| heading.1).collect(),
            headnotes, headnote_marks: reading.headnote_marks, parts: parts.into_iter().map(|part| part.1).collect(), front: reading.front,
            furniture: reading.furniture, unmatched_calls: reading.unmatched_calls, tables: reading.tables, defined_terms: reading.defined_terms,
            quotes: reading.quotes, instruments: reading.instruments, record_gaps: reading.record_gaps, citations: reading.citations,
            page_series: reading.page_series, outcomes: reading.outcomes, document_kind: reading.document_kind,
            page_witness: witness(census, "page_witness"), note_witness: witness(census, "note_witness"), heading_witness: witness(census, "heading_witness"),
            paragraph_witness: witness(census, "paragraph_witness"), paragraphs_run_on: witness(census, "paragraphs_run_on"), paragraphs_split: witness(census, "paragraphs_split"),
            union_test: witness(census, "union_test"), union_samples: witness(census, "union_samples"), titles_not_held: witness(census, "titles_not_held"),
            marked_titles: HashSet::new(), elsewhere: Vec::new(), quoted_elsewhere: HashSet::new(), printed_elsewhere: Vec::new(), cuts: Vec::new(),
            other_opinion: reading.other_opinion, cited: structure.cited_authorities.clone(),
        }
    }
}
