use crate::{
    javascript_whitespace,
    locator::normalize_numbered_section_locator,
    public_structure_label,
    text::{trim_javascript_whitespace as js_trim, JS_WHITESPACE_CLASS as JS_WS},
    Derivation, DetectionProfile, DocumentBlock, DocumentKind, DocumentOrigin, DocumentStructure,
    InstrumentCrossReferenceGraph, InstrumentCrossReferenceStatus, NodeKind, ScalarText,
    StructureNode,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    OnceLock,
};

mod text_fragment;
pub use text_fragment::text_fragment_directives;

fn regex(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("query regex must compile"))
}

fn regex_parts(parts: &[&str], cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(&parts.concat()).expect("query regex must compile"))
}

fn js_regex(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| {
        Regex::new(&pattern.replace(r"\s", JS_WS)).expect("query regex must compile")
    })
}

fn equal_fold(left: &str, right: &str) -> bool {
    if left.is_ascii() && right.is_ascii() {
        left.eq_ignore_ascii_case(right)
    } else {
        left.to_lowercase() == right.to_lowercase()
    }
}

fn context(value: usize) -> usize {
    value.min(2)
}

fn slice_utf16<'a>(text: &ScalarText<'a>, start: usize, end: usize) -> &'a str {
    let Some(start) = text.byte_at_utf16(start) else {
        return "";
    };
    let Some(end) = text.byte_at_utf16(end) else {
        return "";
    };
    text.value.get(start..end).unwrap_or("")
}

#[derive(Clone, Serialize)]
pub struct MaterializedDocumentBlock {
    #[serde(flatten)]
    pub block: DocumentBlock,
    pub text: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentLookupStatus {
    Found,
    NotFound,
    Unavailable,
    Ambiguous,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLookup {
    pub status: DocumentLookupStatus,
    pub requested_label: String,
    pub matches: Vec<String>,
    pub block: Option<MaterializedDocumentBlock>,
    pub before: Vec<MaterializedDocumentBlock>,
    pub after: Vec<MaterializedDocumentBlock>,
}

#[derive(Serialize)]
pub struct DocumentRangeLookup {
    pub selected: Vec<MaterializedDocumentBlock>,
    pub before: Vec<MaterializedDocumentBlock>,
    pub after: Vec<MaterializedDocumentBlock>,
}

#[derive(Clone, Serialize)]
pub struct DocumentWordSpan {
    pub word: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy)]
struct WordOffset {
    start: usize,
    end: usize,
}

struct SearchIndex {
    tokens: Vec<WordOffset>,
    postings: HashMap<u64, Vec<u32>>,
}

impl SearchIndex {
    fn with_scalar(text: &str, scalar: &ScalarText<'_>) -> Self {
        let (tokens, hashes) = word_offsets(text, scalar);
        let mut postings = HashMap::<u64, Vec<u32>>::new();
        for (position, hash) in hashes.into_iter().enumerate() {
            postings.entry(hash).or_default().push(position as u32);
        }
        Self { tokens, postings }
    }
}

fn word_hash(word: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    for character in word.chars() {
        character.hash(&mut hash);
    }
    hash.finish()
}

fn normalized_word_hash(word: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    for character in word.chars().flat_map(char::to_lowercase) {
        character.hash(&mut hash);
    }
    hash.finish()
}

fn normalized_word_matches(word: &str, normalized: &str) -> bool {
    if word.is_ascii() {
        word.bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .eq(normalized.bytes())
    } else {
        word.to_lowercase() == normalized
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhraseOptions {
    pub start: Option<usize>,
    pub end: Option<usize>,
    #[serde(default)]
    pub same_line: bool,
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhraseSpan {
    pub start: usize,
    pub end: usize,
    pub first_word: usize,
    pub last_word: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageSpan {
    pub ordinal: usize,
    pub pdf_page: Option<usize>,
    pub printed_label: Option<String>,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PageMapSource {
    Artifact,
    Markers,
    Unpaginated,
    Unindexed,
}

#[derive(Serialize)]
pub struct PageMap {
    pub pages: Vec<PageSpan>,
    pub source: PageMapSource,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PageSense {
    Pdf,
    Printed,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PageLookup {
    Found {
        page: PageSpan,
        #[serde(rename = "matchedOn")]
        matched_on: PageSense,
        text: String,
    },
    NoPages,
    NotFound {
        requested: String,
        sense: PageSense,
        count: usize,
        first: Option<String>,
        last: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DocumentAddress {
    Section { locator: String },
    Page { spec: String },
    Offset { start: usize },
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowDirection {
    None,
    Out,
    In,
    Both,
}

#[derive(Serialize)]
pub struct GraphScope {
    pub seed: MaterializedDocumentBlock,
    pub nodes: Vec<GraphScopeNode>,
    pub depth: usize,
}

#[derive(Serialize)]
pub struct GraphScopeNode {
    #[serde(flatten)]
    pub block: MaterializedDocumentBlock,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<Vec<MaterializedDocumentBlock>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProjectionOrder {
    Case,
    Legislation,
    Position,
    StablePosition,
    Native,
}

#[derive(Clone, Copy)]
struct BlockPosition {
    node: usize,
    prose: Option<usize>,
    parent: Option<usize>,
}

fn projection_order(profile: Option<DetectionProfile>) -> ProjectionOrder {
    match profile {
        Some(DetectionProfile::CaseRootedComplete) => ProjectionOrder::Case,
        Some(DetectionProfile::CaseContiguousComplete | DetectionProfile::CaseLossy) | None => {
            ProjectionOrder::Position
        }
        Some(DetectionProfile::Legislation) => ProjectionOrder::Legislation,
        Some(DetectionProfile::Instrument) => ProjectionOrder::Native,
        Some(DetectionProfile::Journal) => ProjectionOrder::StablePosition,
    }
}

fn projected_kind(node: &StructureNode) -> Option<DocumentKind> {
    Some(match node.kind {
        NodeKind::Paragraph | NodeKind::Prose => DocumentKind::Paragraph,
        NodeKind::Page => DocumentKind::Page,
        NodeKind::Section => DocumentKind::Section,
        NodeKind::Footnote => DocumentKind::Footnote,
        NodeKind::Table => DocumentKind::Table,
        NodeKind::Row => DocumentKind::Row,
        NodeKind::Cell => DocumentKind::Cell,
        NodeKind::Heading | NodeKind::Endnote | NodeKind::List | NodeKind::ListItem => return None,
    })
}

fn position_label<'a>(document: &'a DocumentStructure, position: &BlockPosition) -> Cow<'a, str> {
    let node = &document.nodes[position.node];
    let label = position.prose.map_or_else(
        || Cow::Borrowed(node.label.as_deref().expect("projected node has a label")),
        |number| Cow::Owned(format!("par{number}")),
    );
    if projection_order(document.profile) == ProjectionOrder::Legislation {
        Cow::Owned(public_structure_label(&label))
    } else {
        label
    }
}

fn parent_label<'a>(
    document: &'a DocumentStructure,
    position: &BlockPosition,
) -> Option<Cow<'a, str>> {
    let label = position
        .parent
        .and_then(|parent| document.nodes[parent].label.as_deref())?;
    Some(
        if projection_order(document.profile) == ProjectionOrder::Legislation {
            Cow::Owned(public_structure_label(label))
        } else {
            Cow::Borrowed(label)
        },
    )
}

fn projected_positions(document: &DocumentStructure) -> Vec<BlockPosition> {
    let mut prose = 0;
    // Insertion order deliberately preserves the old reverse lookup when IDs repeat.
    let nodes_by_id = document
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut positions = document
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            projected_kind(node)?;
            let prose_number = if node.kind == NodeKind::Prose {
                prose += 1;
                Some(prose)
            } else {
                node.label.as_ref()?;
                None
            };
            Some(BlockPosition {
                node: index,
                prose: prose_number,
                parent: node
                    .parent_id
                    .as_deref()
                    .and_then(|parent| nodes_by_id.get(parent).copied()),
            })
        })
        .collect::<Vec<_>>();
    match projection_order(document.profile) {
        ProjectionOrder::StablePosition => positions.sort_by_key(|position| {
            let range = document.query_range(&document.nodes[position.node]);
            (range.start, range.end)
        }),
        ProjectionOrder::Position => positions.sort_by(|left, right| {
            let left_range = document.query_range(&document.nodes[left.node]);
            let right_range = document.query_range(&document.nodes[right.node]);
            (
                left_range.start,
                left_range.end,
                position_label(document, left),
            )
                .cmp(&(
                    right_range.start,
                    right_range.end,
                    position_label(document, right),
                ))
        }),
        ProjectionOrder::Legislation => positions.sort_by(|left, right| {
            let left_range = document.query_range(&document.nodes[left.node]);
            let right_range = document.query_range(&document.nodes[right.node]);
            left_range
                .start
                .cmp(&right_range.start)
                .then_with(|| right_range.end.cmp(&left_range.end))
                .then_with(|| position_label(document, left).cmp(&position_label(document, right)))
        }),
        ProjectionOrder::Case | ProjectionOrder::Native => {}
    }
    if projection_order(document.profile) == ProjectionOrder::Legislation {
        let owners = positions
            .iter()
            .enumerate()
            .map(|(index, position)| (position_label(document, position).into_owned(), index))
            .collect::<HashMap<_, _>>();
        for index in 0..positions.len() {
            let owner = owners
                .get(position_label(document, &positions[index]).as_ref())
                .copied()
                .unwrap_or(index);
            let mut seen = HashSet::new();
            let parent = std::iter::successors(positions[owner].parent, |&parent| {
                let label = public_structure_label(document.nodes[parent].label.as_deref()?);
                seen.insert(label.clone())
                    .then(|| {
                        owners
                            .get(&label)
                            .and_then(|owner| positions[*owner].parent)
                    })
                    .flatten()
            })
            .last();
            positions[index].parent = parent;
        }
    }
    positions
}

#[derive(Serialize)]
struct DocumentAnchor<'a> {
    kind: DocumentKind,
    label: Cow<'a, str>,
    start: usize,
    end: usize,
    #[serde(rename = "parentLabel", skip_serializing_if = "Option::is_none")]
    parent_label: Option<Cow<'a, str>>,
}

pub struct DocumentQuery {
    queries: AtomicUsize,
    blocks: OnceLock<Vec<BlockPosition>>,
    search: OnceLock<SearchIndex>,
    line_breaks: OnceLock<Vec<usize>>,
}

impl Default for DocumentQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentQuery {
    pub fn new() -> Self {
        Self {
            queries: AtomicUsize::new(0),
            blocks: OnceLock::new(),
            search: OnceLock::new(),
            line_breaks: OnceLock::new(),
        }
    }

    fn positions<'a>(&'a self, document: &DocumentStructure) -> &'a [BlockPosition] {
        self.blocks.get_or_init(|| projected_positions(document))
    }

    fn positions_of_kind<'a>(
        &'a self,
        document: &DocumentStructure,
        kind: DocumentKind,
    ) -> Vec<&'a BlockPosition> {
        self.positions(document)
            .iter()
            .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind))
            .collect()
    }

    fn block(&self, document: &DocumentStructure, position: &BlockPosition) -> DocumentBlock {
        let node = &document.nodes[position.node];
        let range = document.query_range(node);
        let mut block = DocumentBlock::new(
            projected_kind(node).expect("projected node kind"),
            position_label(document, position),
            range.start,
            range.end,
            if node.source == Derivation::Native {
                DocumentOrigin::Native
            } else {
                DocumentOrigin::Heuristic
            },
        );
        if projection_order(document.profile) == ProjectionOrder::StablePosition
            && node.kind != NodeKind::Prose
        {
            block.field_order = crate::document_block::BlockFieldOrder::EndLast;
        }
        block.aliases = node.aliases.clone().unwrap_or_default();
        block.anchor.clone_from(&node.anchor);
        block.parent_label = parent_label(document, position).map(Cow::into_owned);
        block
    }

    pub fn anchors<'a>(
        &'a self,
        document: &'a DocumentStructure,
    ) -> impl Iterator<Item = impl Serialize + 'a> + 'a {
        self.positions(document).iter().map(|position| {
            let node = &document.nodes[position.node];
            let range = document.query_range(node);
            DocumentAnchor {
                kind: projected_kind(node).expect("projected node kind"),
                label: position_label(document, position),
                start: range.start,
                end: range.end,
                parent_label: parent_label(document, position),
            }
        })
    }

    fn materialize(
        &self,
        document: &DocumentStructure,
        position: &BlockPosition,
        text: &ScalarText<'_>,
    ) -> MaterializedDocumentBlock {
        let block = self.block(document, position);
        MaterializedDocumentBlock {
            text: js_trim(slice_utf16(text, block.start, block.end)).to_owned(),
            block,
        }
    }

    pub fn blocks<'a>(
        &'a self,
        document: &'a DocumentStructure,
        kind: Option<DocumentKind>,
    ) -> impl Iterator<Item = DocumentBlock> + 'a {
        self.positions(document)
            .iter()
            .filter(move |position| {
                kind.is_none_or(|kind| projected_kind(&document.nodes[position.node]) == Some(kind))
            })
            .map(|position| self.block(document, position))
    }

    fn position_matches(
        &self,
        document: &DocumentStructure,
        position: &BlockPosition,
        label: &str,
    ) -> bool {
        equal_fold(&position_label(document, position), label)
            || document.nodes[position.node]
                .aliases
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|alias| equal_fold(alias, label))
    }

    fn unique_position<'a>(
        &'a self,
        document: &DocumentStructure,
        label: &str,
    ) -> Option<&'a BlockPosition> {
        let mut found = None;
        for position in self.positions(document) {
            let node = &document.nodes[position.node];
            let matches = equal_fold(&position_label(document, position), label)
                || node
                    .aliases
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|alias| equal_fold(alias, label))
                || node
                    .anchor
                    .as_deref()
                    .is_some_and(|anchor| equal_fold(anchor, label));
            if matches {
                if found.is_some() {
                    return None;
                }
                found = Some(position);
            }
        }
        found
    }

    fn last_position_by_label<'a>(
        &'a self,
        document: &DocumentStructure,
        label: &str,
    ) -> Option<&'a BlockPosition> {
        self.positions(document)
            .iter()
            .rev()
            .find(|position| position_label(document, position) == label)
    }

    pub fn subtree_labels(&self, document: &DocumentStructure, seed_label: &str) -> Vec<String> {
        let mut labels = Vec::new();
        for block in self.positions(document) {
            let mut current = Some(block);
            let mut seen = HashSet::new();
            while let Some(candidate) = current {
                let label = position_label(document, candidate);
                if !seen.insert(label.to_string()) {
                    break;
                }
                if label == seed_label {
                    labels.push(position_label(document, block).into_owned());
                    break;
                }
                current = parent_label(document, candidate)
                    .as_deref()
                    .and_then(|parent| self.last_position_by_label(document, parent));
            }
        }
        labels
    }

    pub fn has_native_ancestor(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        label: &str,
    ) -> bool {
        let mut current = self
            .unique_position(document, label)
            .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind));
        let mut seen = HashSet::new();
        while let Some(position) = current {
            let label = position_label(document, position);
            if !seen.insert(label.into_owned()) {
                return false;
            }
            if document.nodes[position.node].source == Derivation::Native {
                return true;
            }
            current = parent_label(document, position)
                .as_deref()
                .and_then(|parent| self.unique_position(document, parent))
                .filter(|candidate| projected_kind(&document.nodes[candidate.node]) == Some(kind));
        }
        false
    }

    pub fn has_origin(&self, document: &DocumentStructure, origin: DocumentOrigin) -> bool {
        document
            .nodes
            .iter()
            .filter(|node| {
                projected_kind(node).is_some()
                    && (node.kind == NodeKind::Prose || node.label.is_some())
            })
            .any(|node| {
                let native = node.source == Derivation::Native;
                native == (origin == DocumentOrigin::Native)
            })
    }

    pub fn lookup(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        locator: &str,
        context_blocks: usize,
    ) -> DocumentLookup {
        self.lookup_with_text(document, kind, locator, context_blocks, None)
    }

    fn lookup_with_text(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        locator: &str,
        context_blocks: usize,
        text: Option<&ScalarText<'_>>,
    ) -> DocumentLookup {
        let requested = self.requested_label(document, kind, locator);
        self.lookup_label_with_text(document, kind, &requested, context_blocks, text)
    }

    fn requested_label(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        locator: &str,
    ) -> String {
        let exact = js_trim(locator);
        if self
            .positions(document)
            .iter()
            .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind))
            .any(|position| equal_fold(&position_label(document, position), exact))
        {
            return exact.to_owned();
        }
        let normalized = normalize_document_locator(kind, locator);
        if !normalized.is_empty() {
            return normalized;
        }
        self.positions(document)
            .iter()
            .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind))
            .any(|position| {
                let node = &document.nodes[position.node];
                node.anchor
                    .as_deref()
                    .is_some_and(|anchor| equal_fold(anchor, exact))
                    || node
                        .aliases
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|alias| equal_fold(alias, exact))
            })
            .then(|| exact.to_owned())
            .unwrap_or_default()
    }

    pub fn lookup_label(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        requested_label: &str,
        context_blocks: usize,
    ) -> DocumentLookup {
        self.lookup_label_with_text(document, kind, requested_label, context_blocks, None)
    }

    fn lookup_label_with_text(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        requested_label: &str,
        context_blocks: usize,
        text: Option<&ScalarText<'_>>,
    ) -> DocumentLookup {
        let available = self.positions_of_kind(document, kind);
        let empty = |status| DocumentLookup {
            status,
            requested_label: requested_label.to_owned(),
            matches: Vec::new(),
            block: None,
            before: Vec::new(),
            after: Vec::new(),
        };
        if requested_label.is_empty() || available.is_empty() {
            return empty(DocumentLookupStatus::Unavailable);
        }
        let selected = self
            .unique_position(document, requested_label)
            .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind));
        let Some(selected) = selected else {
            let matches = available
                .iter()
                .filter(|position| self.position_matches(document, position, requested_label))
                .map(|position| position_label(document, position).into_owned())
                .collect::<Vec<_>>();
            return DocumentLookup {
                status: if matches.is_empty() {
                    DocumentLookupStatus::NotFound
                } else {
                    DocumentLookupStatus::Ambiguous
                },
                matches,
                ..empty(DocumentLookupStatus::NotFound)
            };
        };
        let order = available
            .iter()
            .position(|candidate| candidate.node == selected.node)
            .unwrap_or(0);
        let context = context(context_blocks);
        let owned_text;
        let text = if let Some(text) = text {
            text
        } else {
            owned_text = ScalarText::new(document.query_text());
            &owned_text
        };
        DocumentLookup {
            status: DocumentLookupStatus::Found,
            requested_label: requested_label.to_owned(),
            matches: vec![position_label(document, selected).into_owned()],
            block: Some(self.materialize(document, selected, text)),
            before: available[order.saturating_sub(context)..order]
                .iter()
                .map(|position| self.materialize(document, position, text))
                .collect(),
            after: available[order + 1..(order + 1 + context).min(available.len())]
                .iter()
                .map(|position| self.materialize(document, position, text))
                .collect(),
        }
    }

    pub fn read_range(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        from: &str,
        to: &str,
        context_blocks: usize,
    ) -> Option<DocumentRangeLookup> {
        let text = ScalarText::new(document.query_text());
        let available = self.positions_of_kind(document, kind);
        let resolve = |locator: &str| {
            let label = self.requested_label(document, kind, locator);
            let position = self
                .unique_position(document, &label)
                .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind))?;
            available
                .iter()
                .position(|candidate| candidate.node == position.node)
        };
        let first_index = resolve(from)?;
        let last_index = resolve(to)?;
        let (low, high) = if first_index <= last_index {
            (first_index, last_index)
        } else {
            (last_index, first_index)
        };
        let context = context(context_blocks);
        Some(DocumentRangeLookup {
            selected: self.materialize_leaf_blocks(document, &available[low..=high], &text),
            before: self.materialize_leaf_blocks(
                document,
                &available[low.saturating_sub(context)..low],
                &text,
            ),
            after: self.materialize_leaf_blocks(
                document,
                &available[high + 1..(high + 1 + context).min(available.len())],
                &text,
            ),
        })
    }

    fn contained_leaf_blocks<'a>(
        &'a self,
        document: &DocumentStructure,
        blocks: &[&BlockPosition],
    ) -> Vec<&'a BlockPosition> {
        let Some(kind) = blocks
            .first()
            .and_then(|position| projected_kind(&document.nodes[position.node]))
        else {
            return Vec::new();
        };
        let mut ranges = blocks
            .iter()
            .map(|position| document.query_range(&document.nodes[position.node]))
            .map(|range| (range.start, range.end))
            .collect::<Vec<_>>();
        ranges.sort_unstable();
        let mut maximum_ends = Vec::<usize>::with_capacity(ranges.len());
        for &(_, end) in &ranges {
            maximum_ends.push(maximum_ends.last().copied().unwrap_or(0).max(end));
        }
        let contained = self
            .positions(document)
            .iter()
            .filter(|position| {
                if projected_kind(&document.nodes[position.node]) != Some(kind) {
                    return false;
                }
                let range = document.query_range(&document.nodes[position.node]);
                let count = ranges.partition_point(|(start, _)| *start <= range.start);
                count > 0 && maximum_ends[count - 1] >= range.end
            })
            .collect::<Vec<_>>();
        if kind != DocumentKind::Section || contained.len() <= 1 {
            return contained;
        }
        let mut ordered = (0..contained.len()).collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|&index| {
            let range = document.query_range(&document.nodes[contained[index].node]);
            (range.start, std::cmp::Reverse(range.end))
        });
        let mut stack = Vec::<usize>::new();
        let mut parents = HashSet::new();
        for index in ordered {
            let candidate = document.query_range(&document.nodes[contained[index].node]);
            while stack.last().is_some_and(|&parent| {
                document
                    .query_range(&document.nodes[contained[parent].node])
                    .end
                    < candidate.end
            }) {
                stack.pop();
            }
            for &parent in &stack {
                let parent_range = document.query_range(&document.nodes[contained[parent].node]);
                if parent_range.end >= candidate.end
                    && (parent_range.start < candidate.start || parent_range.end > candidate.end)
                {
                    parents.insert(contained[parent].node);
                }
            }
            stack.push(index);
        }
        contained
            .into_iter()
            .filter(|candidate| !parents.contains(&candidate.node))
            .collect()
    }

    fn materialize_leaf_blocks(
        &self,
        document: &DocumentStructure,
        blocks: &[&BlockPosition],
        text: &ScalarText<'_>,
    ) -> Vec<MaterializedDocumentBlock> {
        self.contained_leaf_blocks(document, blocks)
            .into_iter()
            .map(|position| self.materialize(document, position, text))
            .filter(|unit| !unit.text.is_empty())
            .collect()
    }

    pub fn smallest_containing_block(
        &self,
        document: &DocumentStructure,
        start: usize,
        end: usize,
    ) -> Option<MaterializedDocumentBlock> {
        let position = self
            .positions(document)
            .iter()
            .filter(|position| {
                let range = document.query_range(&document.nodes[position.node]);
                range.start <= start && range.end >= end
            })
            .min_by_key(|position| {
                let range = document.query_range(&document.nodes[position.node]);
                range.end - range.start
            })?;
        Some(self.materialize(document, position, &ScalarText::new(document.query_text())))
    }

    fn index_with_text(&self, document: &DocumentStructure, text: &ScalarText<'_>) -> &SearchIndex {
        self.search
            .get_or_init(|| SearchIndex::with_scalar(document.query_text(), text))
    }

    fn line_breaks_with_text(
        &self,
        document: &DocumentStructure,
        text: &ScalarText<'_>,
    ) -> &[usize] {
        self.line_breaks
            .get_or_init(|| collect_line_breaks(document.query_text(), text))
    }

    pub fn phrase_spans(
        &self,
        document: &DocumentStructure,
        words: &[String],
        options: PhraseOptions,
    ) -> Vec<PhraseSpan> {
        let text = ScalarText::new(document.query_text());
        self.phrase_spans_with_text(document, words, options, &text)
    }

    fn phrase_spans_with_text(
        &self,
        document: &DocumentStructure,
        words: &[String],
        options: PhraseOptions,
        text: &ScalarText<'_>,
    ) -> Vec<PhraseSpan> {
        if words.is_empty() {
            return Vec::new();
        }
        let query = self.queries.fetch_add(1, Ordering::Relaxed) + 1;
        if query == 1
            && self.search.get().is_none()
            && options.start.is_none()
            && options.end.is_none()
        {
            return scan_phrase_spans_with_text(document.query_text(), text, words, options);
        }
        indexed_phrase_spans(
            self.index_with_text(document, text),
            self.line_breaks_with_text(document, text),
            text,
            words,
            options,
        )
    }

    pub fn contains_quote(
        &self,
        document: &DocumentStructure,
        quote: &str,
        start: Option<usize>,
        end: Option<usize>,
    ) -> bool {
        !self
            .phrase_spans(
                document,
                &quote_words(quote),
                PhraseOptions {
                    start,
                    end,
                    limit: Some(1),
                    same_line: false,
                },
            )
            .is_empty()
    }

    pub fn page_map(&self, document: &DocumentStructure) -> PageMap {
        let mut pages = Vec::new();
        for position in self.positions(document).iter().filter(|position| {
            projected_kind(&document.nodes[position.node]) == Some(DocumentKind::Page)
        }) {
            let node = &document.nodes[position.node];
            let range = document.query_range(node);
            let pdf_page: Option<usize> = node
                .anchor
                .as_deref()
                .and_then(|anchor| anchor.strip_prefix("page="))
                .filter(|value| {
                    !value.is_empty()
                        && value.len() <= 6
                        && value.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|value| value.parse().ok());
            let printed_label = node
                .aliases
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|alias| js_trim(alias))
                .find(|alias| {
                    !alias.is_empty()
                        && *alias
                            != pdf_page.map_or_else(|| "null".to_owned(), |page| page.to_string())
                })
                .map(str::to_owned);
            pages.push(PageSpan {
                ordinal: pages.len() + 1,
                pdf_page,
                printed_label,
                start: range.start,
                end: range.end,
            });
        }
        pages.sort_by_key(|page| page.start);
        for (index, page) in pages.iter_mut().enumerate() {
            page.ordinal = index + 1;
        }
        if pages.is_empty() {
            return page_map_from_markers(document.query_text());
        }
        PageMap {
            source: PageMapSource::Artifact,
            pages,
        }
    }

    pub fn resolve_page(&self, document: &DocumentStructure, requested: &str) -> PageLookup {
        resolve_page(&self.page_map(document), document.query_text(), requested)
    }

    pub fn structure_block(
        &self,
        document: &DocumentStructure,
        locator: &str,
        context_blocks: usize,
    ) -> DocumentLookup {
        let direct = js_trim(locator).to_lowercase();
        if direct.starts_with("table:") {
            let kind = if direct.contains("/col:") {
                DocumentKind::Cell
            } else if direct.contains("/row:") {
                DocumentKind::Row
            } else {
                DocumentKind::Table
            };
            return self.lookup_label(document, kind, &direct, context_blocks);
        }
        let normalized = normalize_document_locator(DocumentKind::Section, locator);
        if !normalized.is_empty() {
            let found =
                self.lookup_label(document, DocumentKind::Section, &normalized, context_blocks);
            if !matches!(found.status, DocumentLookupStatus::NotFound) {
                return found;
            }
        }
        self.lookup_label(document, DocumentKind::Section, &direct, context_blocks)
    }

    pub fn graph_scope(
        &self,
        document: &DocumentStructure,
        graph: &InstrumentCrossReferenceGraph,
        seed_label: &str,
        follow: FollowDirection,
        depth: usize,
        include_descendants: bool,
        include_units: bool,
    ) -> Option<GraphScope> {
        let wanted = js_trim(seed_label).to_lowercase();
        let seed = self
            .positions(document)
            .iter()
            .find(|position| position_label(document, position).to_lowercase() == wanted)?;
        let seed_label = position_label(document, seed).into_owned();
        let limit = depth.min(3);
        let initial = if include_descendants {
            self.subtree_labels(document, &seed_label)
        } else {
            vec![seed_label.clone()]
        };
        let initial_set = initial.iter().cloned().collect::<HashSet<_>>();
        let mut reached = vec![*seed];
        let mut frontier = initial;
        let mut hops = 0;
        while follow != FollowDirection::None && hops < limit && !frontier.is_empty() {
            let in_frontier = frontier.iter().map(String::as_str).collect::<HashSet<_>>();
            let mut next = Vec::new();
            for edge in &graph.edges {
                if edge.status != InstrumentCrossReferenceStatus::Resolved || edge.self_loop {
                    continue;
                }
                let forward = matches!(follow, FollowDirection::Out | FollowDirection::Both)
                    && edge
                        .source_label
                        .as_deref()
                        .is_some_and(|label| in_frontier.contains(label));
                let backward = matches!(follow, FollowDirection::In | FollowDirection::Both)
                    && edge
                        .target_label
                        .as_deref()
                        .is_some_and(|label| in_frontier.contains(label));
                let other = if forward {
                    edge.target_label.as_deref()
                } else if backward {
                    edge.source_label.as_deref()
                } else {
                    None
                };
                let Some(other) = other else { continue };
                if initial_set.contains(other)
                    || reached
                        .iter()
                        .any(|position| position_label(document, position) == other)
                {
                    continue;
                }
                let Some(position) = self
                    .positions(document)
                    .iter()
                    .find(|position| position_label(document, position) == other)
                else {
                    continue;
                };
                reached.push(*position);
                next.push(other.to_owned());
            }
            frontier = next;
            hops += 1;
        }
        let mut rest = reached
            .into_iter()
            .filter(|position| position.node != seed.node)
            .collect::<Vec<_>>();
        rest.sort_by_key(|position| document.query_range(&document.nodes[position.node]).start);
        let text = ScalarText::new(document.query_text());
        let seed = self.materialize(document, seed, &text);
        Some(GraphScope {
            seed,
            nodes: rest
                .iter()
                .map(|position| {
                    let materialized = self.materialize(document, position, &text);
                    let units = include_units
                        .then(|| self.materialize_leaf_blocks(document, &[position], &text));
                    let units = units.filter(|units| {
                        units.len() != 1
                            || units[0].block.start != materialized.block.start
                            || units[0].block.end != materialized.block.end
                    });
                    GraphScopeNode {
                        block: materialized,
                        units,
                    }
                })
                .collect(),
            depth: hops.min(limit),
        })
    }
}

pub fn normalize_document_locator(kind: DocumentKind, locator: &str) -> String {
    static FOOTNOTE: OnceLock<Regex> = OnceLock::new();
    static PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    static PAGE: OnceLock<Regex> = OnceLock::new();
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    static CANONICAL_PREFIX: OnceLock<Regex> = OnceLock::new();
    static HEADING: OnceLock<Regex> = OnceLock::new();
    static NON_TITLE: OnceLock<Regex> = OnceLock::new();
    let value = js_trim(locator);
    let numbered = |capture: &regex::Captures<'_>, prefix: &str| {
        format!("{prefix}{}", capture[1].parse::<usize>().unwrap_or(0))
    };
    match kind {
        DocumentKind::Footnote => {
            return regex_parts(
                &[
                    r"(?iu)^(?:fn|footnotes?|notes?)?(?:",
                    JS_WS,
                    r"|[#.])*(\d{1,5})$",
                ],
                &FOOTNOTE,
            )
            .captures(value)
            .map_or_else(String::new, |capture| numbered(&capture, "fn"))
        }
        DocumentKind::Paragraph => {
            return regex_parts(
                &[
                    r"(?iu)^(?:\[",
                    JS_WS,
                    r"*)?(?:paras?\.?|paragraphs?)?",
                    JS_WS,
                    r"*(\d{1,4})(?:",
                    JS_WS,
                    r"*\])?$",
                ],
                &PARAGRAPH,
            )
            .captures(value)
            .map_or_else(String::new, |capture| numbered(&capture, "par"))
        }
        DocumentKind::Page => {
            return regex_parts(&[r"(?iu)^(?:pages?|pp?\.)?", JS_WS, r"*(\d{1,4})$"], &PAGE)
                .captures(value)
                .map_or_else(String::new, |capture| numbered(&capture, "page"))
        }
        DocumentKind::Section => {}
        _ => return String::new(),
    }
    let without_prefix =
        regex_parts(&[r"(?iu)^(?:sections?|ss?\.?)", JS_WS, "+"], &PREFIX).replace(value, "");
    let without_prefix =
        regex(r"(?iu)^sec([\p{L}\p{N}])", &CANONICAL_PREFIX).replace(&without_prefix, "$1");
    let numbered = normalize_numbered_section_locator(&without_prefix);
    if !numbered.is_empty() {
        return numbered;
    }
    let heading = without_prefix
        .trim_end_matches(|character| character == '.' || javascript_whitespace(character));
    if regex(r"^(?:[IVXLCDM]+|[A-Z])$", &HEADING).is_match(heading) {
        return format!("sec{heading}");
    }
    let lowercase = heading.to_lowercase();
    let normalized = regex(r"[^\p{L}\p{N}]+", &NON_TITLE).replace_all(&lowercase, " ");
    let title = js_trim(&normalized);
    if title.is_empty() {
        String::new()
    } else {
        format!("sectitle:{title}")
    }
}

fn tokenize_with_scalar(text: &str, scalar: &ScalarText<'_>) -> Vec<DocumentWordSpan> {
    static WORD: OnceLock<Regex> = OnceLock::new();
    regex(r"[\p{L}\p{N}]+(?:['’][\p{L}\p{N}]+)*", &WORD)
        .find_iter(text)
        .map(|found| DocumentWordSpan {
            word: found.as_str().to_lowercase(),
            start: scalar.utf16_at_byte(found.start()).expect("token boundary"),
            end: scalar.utf16_at_byte(found.end()).expect("token boundary"),
        })
        .collect()
}

fn word_offsets(text: &str, scalar: &ScalarText<'_>) -> (Vec<WordOffset>, Vec<u64>) {
    static WORD: OnceLock<Regex> = OnceLock::new();
    let mut offsets = Vec::new();
    let mut hashes = Vec::new();
    for found in regex(r"[\p{L}\p{N}]+(?:['â€™][\p{L}\p{N}]+)*", &WORD).find_iter(text) {
        offsets.push(WordOffset {
            start: scalar.utf16_at_byte(found.start()).expect("token boundary"),
            end: scalar.utf16_at_byte(found.end()).expect("token boundary"),
        });
        hashes.push(normalized_word_hash(found.as_str()));
    }
    (offsets, hashes)
}

pub fn tokenize_source_text(text: &str) -> Vec<DocumentWordSpan> {
    tokenize_with_scalar(text, &ScalarText::new(text))
}

pub fn quote_text(value: &str) -> String {
    static BRACKET_LETTER: OnceLock<Regex> = OnceLock::new();
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    static ELISION: OnceLock<Regex> = OnceLock::new();
    let value =
        js_trim(value).trim_matches(|character| matches!(character, '"' | '\'' | '“' | '”'));
    let value = regex(r"\[([A-Za-z])\]([A-Za-z])", &BRACKET_LETTER).replace_all(value, "$1$2");
    let value = regex(r"\[([^\]]+)\]", &BRACKETS).replace_all(&value, "$1");
    let value = regex(r"\.{3}|…", &ELISION).replace_all(&value, " ");
    let mut normalized = String::with_capacity(value.len());
    let mut separating = false;
    for character in value.chars() {
        if matches!(
            character,
            ' ' | '\t' | '\r' | '\n' | '\u{000c}' | '\u{000b}'
        ) {
            separating = !normalized.is_empty();
        } else {
            if separating {
                normalized.push(' ');
            }
            normalized.push(character);
            separating = false;
        }
    }
    normalized
}

pub fn quote_words(quote: &str) -> Vec<String> {
    tokenize_source_text(&quote_text(quote))
        .into_iter()
        .map(|token| token.word)
        .collect()
}

fn token_index_at_or_after(tokens: &[WordOffset], offset: usize) -> usize {
    tokens.partition_point(|token| token.start < offset)
}

fn collect_line_breaks(text: &str, scalar: &ScalarText<'_>) -> Vec<usize> {
    text.match_indices('\n')
        .map(|(byte, _)| {
            scalar
                .utf16_at_byte(byte)
                .expect("line break is a scalar boundary")
        })
        .collect()
}

fn crosses_line_break(line_breaks: &[usize], start: usize, end: usize) -> bool {
    let at = line_breaks.partition_point(|offset| *offset < start);
    at < line_breaks.len() && line_breaks[at] < end
}

fn indexed_phrase_spans(
    index: &SearchIndex,
    line_breaks: &[usize],
    text: &ScalarText<'_>,
    words: &[String],
    options: PhraseOptions,
) -> Vec<PhraseSpan> {
    let from = options
        .start
        .map_or(0, |offset| token_index_at_or_after(&index.tokens, offset));
    let until = options.end.map_or(index.tokens.len(), |offset| {
        token_index_at_or_after(&index.tokens, offset)
    });
    let limit = options.limit.unwrap_or(usize::MAX);
    let hashes = words.iter().map(|word| word_hash(word)).collect::<Vec<_>>();
    let Some((anchor, _)) = hashes
        .iter()
        .enumerate()
        .map(|(offset, hash)| (offset, index.postings.get(hash).map_or(0, Vec::len)))
        .filter(|(_, size)| *size > 0)
        .min_by_key(|(_, size)| *size)
    else {
        return Vec::new();
    };
    if hashes.iter().any(|hash| !index.postings.contains_key(hash)) {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for &position in &index.postings[&hashes[anchor]] {
        let position = position as usize;
        let Some(start) = position.checked_sub(anchor) else {
            continue;
        };
        if start < from {
            continue;
        }
        if start + words.len() > until {
            break;
        }
        if index.tokens[start..start + words.len()]
            .iter()
            .zip(words)
            .any(|(token, word)| {
                !normalized_word_matches(slice_utf16(text, token.start, token.end), word)
            })
        {
            continue;
        }
        let first = &index.tokens[start];
        let last = &index.tokens[start + words.len() - 1];
        if options.same_line && crosses_line_break(line_breaks, first.start, last.end) {
            continue;
        }
        spans.push(PhraseSpan {
            start: first.start,
            end: last.end,
            first_word: start,
            last_word: start + words.len() - 1,
        });
        if spans.len() >= limit {
            break;
        }
    }
    spans
}

fn scan_phrase_spans_with_text(
    text: &str,
    scalar: &ScalarText<'_>,
    words: &[String],
    options: PhraseOptions,
) -> Vec<PhraseSpan> {
    static WORD: OnceLock<Regex> = OnceLock::new();
    let size = words.len();
    let limit = options.limit.unwrap_or(usize::MAX);
    let mut ring = vec![
        DocumentWordSpan {
            word: String::new(),
            start: 0,
            end: 0
        };
        size
    ];
    let mut spans = Vec::new();
    let mut seen = 0;
    for found in regex(r"[\p{L}\p{N}]+(?:['’][\p{L}\p{N}]+)*", &WORD).find_iter(text) {
        let slot = &mut ring[seen % size];
        slot.word = found.as_str().to_lowercase();
        slot.start = scalar.utf16_at_byte(found.start()).expect("token boundary");
        slot.end = scalar.utf16_at_byte(found.end()).expect("token boundary");
        seen += 1;
        if seen < size
            || (0..size).any(|offset| ring[(seen - size + offset) % size].word != words[offset])
        {
            continue;
        }
        let first = &ring[(seen - size) % size];
        let last = &ring[(seen - 1) % size];
        if options.start.is_some_and(|start| first.start < start)
            || options.end.is_some_and(|end| last.start >= end)
        {
            continue;
        }
        if options.same_line {
            let start_byte = scalar.byte_at_utf16(first.start).expect("token boundary");
            let end_byte = scalar.byte_at_utf16(last.end).expect("token boundary");
            if text[start_byte..end_byte].contains('\n') {
                continue;
            }
        }
        spans.push(PhraseSpan {
            start: first.start,
            end: last.end,
            first_word: seen - size,
            last_word: seen - 1,
        });
        if spans.len() >= limit {
            break;
        }
    }
    spans
}

fn scan_phrase_spans(text: &str, words: &[String], options: PhraseOptions) -> Vec<PhraseSpan> {
    scan_phrase_spans_with_text(text, &ScalarText::new(text), words, options)
}

pub fn phrase_spans(text: &str, words: &[String], options: PhraseOptions) -> Vec<PhraseSpan> {
    if words.is_empty() {
        Vec::new()
    } else {
        scan_phrase_spans(text, words, options)
    }
}

pub fn page_map_from_markers(text: &str) -> PageMap {
    static MARKER: OnceLock<Regex> = OnceLock::new();
    let scalar = ScalarText::new(text);
    let mut pages = Vec::<PageSpan>::new();
    for capture in regex(r"(?m)^\[page ([^\]\n]{1,40})\]$", &MARKER).captures_iter(text) {
        let label = js_trim(&capture[1]);
        if label.is_empty() {
            continue;
        }
        let start = scalar
            .utf16_at_byte(capture.get(0).expect("full match").start())
            .expect("marker boundary");
        if let Some(previous) = pages.last_mut() {
            previous.end = start;
        }
        let pdf_page = (label.len() <= 6 && label.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| label.parse().ok())
            .flatten();
        pages.push(PageSpan {
            ordinal: pages.len() + 1,
            pdf_page,
            printed_label: Some(label.to_owned()),
            start,
            end: scalar.utf16_len(),
        });
    }
    PageMap {
        source: if pages.is_empty() {
            PageMapSource::Unpaginated
        } else {
            PageMapSource::Markers
        },
        pages,
    }
}

pub fn resolve_page(map: &PageMap, text: &str, requested: &str) -> PageLookup {
    static QUALIFIED: OnceLock<Regex> = OnceLock::new();
    if map.pages.is_empty() {
        return PageLookup::NoPages;
    }
    let raw = js_trim(requested);
    let qualified = regex_parts(
        &[r"(?iu)^(pdf|printed)", JS_WS, r"*[:=]", JS_WS, r"*(.+)$"],
        &QUALIFIED,
    )
    .captures(raw);
    let wanted = js_trim(qualified.as_ref().map_or(raw, |capture| &capture[2]));
    let sense = qualified
        .as_ref()
        .map(|capture| capture[1].to_lowercase())
        .map_or_else(
            || {
                if !wanted.is_empty()
                    && wanted.len() <= 6
                    && wanted.bytes().all(|byte| byte.is_ascii_digit())
                {
                    PageSense::Pdf
                } else {
                    PageSense::Printed
                }
            },
            |sense| {
                if sense == "pdf" {
                    PageSense::Pdf
                } else {
                    PageSense::Printed
                }
            },
        );
    let page = map.pages.iter().find(|page| match sense {
        PageSense::Pdf => {
            page.pdf_page
                .map_or_else(|| "null".to_owned(), |value| value.to_string())
                == wanted
        }
        PageSense::Printed => page
            .printed_label
            .as_deref()
            .is_some_and(|label| equal_fold(label, wanted)),
    });
    if let Some(page) = page {
        let scalar = ScalarText::new(text);
        return PageLookup::Found {
            page: page.clone(),
            matched_on: sense,
            text: slice_utf16(&scalar, page.start, page.end).to_owned(),
        };
    }
    let describe = |page: &PageSpan| {
        let number = page.pdf_page.unwrap_or(page.ordinal);
        if page.printed_label.as_deref().is_some_and(|label| {
            label
                != page
                    .pdf_page
                    .map_or_else(|| "null".to_owned(), |value| value.to_string())
        }) {
            format!(
                "PDF page {number} (printed \"{}\")",
                page.printed_label.as_deref().unwrap_or_default()
            )
        } else {
            format!("PDF page {number}")
        }
    };
    PageLookup::NotFound {
        requested: raw.to_owned(),
        sense,
        count: map.pages.len(),
        first: map.pages.first().map(&describe),
        last: map.pages.last().map(describe),
    }
}

pub fn parse_address(spec: &str) -> Option<DocumentAddress> {
    static PAGE: OnceLock<Regex> = OnceLock::new();
    static OFFSET: OnceLock<Regex> = OnceLock::new();
    static SECTION: OnceLock<Regex> = OnceLock::new();
    let raw = js_trim(spec);
    if raw.is_empty() {
        return None;
    }
    if let Some(capture) = regex_parts(
        &[
            r"(?iu)^(printed|pdf|page|pg|p)(?-u:\b)",
            JS_WS,
            r"*[:.]?",
            JS_WS,
            r"*(.+)$",
        ],
        &PAGE,
    )
    .captures(raw)
    {
        let qualifier = capture[1].to_lowercase();
        let value = js_trim(&capture[2]);
        return Some(DocumentAddress::Page {
            spec: if matches!(qualifier.as_str(), "pdf" | "printed") {
                format!("{qualifier}:{value}")
            } else {
                value.to_owned()
            },
        });
    }
    if let Some(capture) = regex_parts(
        &[
            r"(?iu)^(?:off|offset)",
            JS_WS,
            r"*[:.]?",
            JS_WS,
            r"*(\d{1,9})$",
        ],
        &OFFSET,
    )
    .captures(raw)
    {
        return Some(DocumentAddress::Offset {
            start: capture[1].parse().expect("bounded digits"),
        });
    }
    let locator = regex_parts(
        &[r"(?iu)^(?:sec|art|sched)", JS_WS, r"*[:.]", JS_WS, "*"],
        &SECTION,
    )
    .replace(raw, "")
    .into_owned();
    Some(DocumentAddress::Section { locator })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Scope, StructureNode};

    #[test]
    fn query_uses_one_unserialized_rendered_plane_and_exact_repeat_search() {
        let mut node = StructureNode::new(
            "par1".to_owned(),
            NodeKind::Paragraph,
            crate::ScalarRange { start: 0, end: 3 },
            "test",
            Derivation::Native,
            None,
        );
        node.label = Some("par1".to_owned());
        let mut document = DocumentStructure::from_scalar_parts(
            "document".to_owned(),
            "raw".to_owned(),
            "revision".to_owned(),
            None,
            Scope::complete(),
            Vec::new(),
            vec![node],
            Vec::new(),
            Vec::new(),
        );
        document.rendered_text = Some("Clean text".to_owned());
        document.nodes[0].rendered_range = Some(crate::ScalarRange { start: 0, end: 10 });

        let query = DocumentQuery::new();
        let block = query.blocks(&document, None).next().unwrap();
        assert_eq!((block.start, block.end), (0, 10));
        let words = quote_words("clean text");
        for spans in [
            query.phrase_spans(&document, &words, PhraseOptions::default()),
            query.phrase_spans(&document, &words, PhraseOptions::default()),
        ] {
            assert_eq!(spans.len(), 1);
            assert_eq!((spans[0].start, spans[0].end), (0, 10));
        }
        assert!(query
            .phrase_spans(
                &document,
                &["Clean".to_owned(), "text".to_owned()],
                PhraseOptions::default(),
            )
            .is_empty());
        let json = serde_json::to_value(&document).unwrap();
        assert_eq!(json["text"], "raw");
        assert!(json.get("rendered_text").is_none());
        assert!(json["nodes"][0].get("rendered_range").is_none());
    }
}
