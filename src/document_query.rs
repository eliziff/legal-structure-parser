use crate::{
    locator::{literal_page_marker, normalize_section_locator},
    public_structure_label,
    text::{equal_fold, trim_javascript_whitespace as js_trim, JS_WHITESPACE_CLASS as JS_WS},
    AuthoritativeTableCell, Derivation, DetectionProfile, DocumentBlock, DocumentKind,
    DocumentOrigin, DocumentStructure, InstrumentCrossReferenceGraph,
    InstrumentCrossReferenceStatus, NodeKind, ScalarRange, ScalarText, StructureNode,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{atomic::AtomicBool, OnceLock};

mod search;
mod text_fragment;
use search::{quote_text, quote_words, PhraseOptions, PhraseSpan};
pub(crate) use search::{tokenize_source_text, DocumentWordSpan};
pub use text_fragment::{text_fragment_plan, TextFragmentPlan, TextFragmentWordInterval};

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

fn context(value: usize) -> usize {
    value.min(2)
}

const READ_OUTPUT_UTF16_BUDGET: usize = 63_000;
const READ_LINE_UTF16_LIMIT: usize = READ_OUTPUT_UTF16_BUDGET - 1_000;

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

#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentTextWindowStatus {
    Ready,
    InvalidLine,
    InvalidCharacter,
    SplitCharacter,
    InvalidRange,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentTextWindowRow<'a> {
    pub line_number: usize,
    pub text: &'a str,
    pub span: [usize; 2],
    pub truncated_start: bool,
    pub truncated_end: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentTextWindow<'a> {
    pub status: DocumentTextWindowStatus,
    pub rows: Vec<DocumentTextWindowRow<'a>>,
    pub next_offset: Option<usize>,
    pub next_start_char: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<usize>,
    pub document_revision: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_end_line: Option<usize>,
}

struct PageSpan {
    pdf_page: Option<usize>,
    printed_label: Option<String>,
    start: usize,
    end: usize,
}

enum PageLookup {
    Found(ScalarRange),
    NoPages,
    NotFound,
}

enum DocumentAddress {
    Section { locator: String },
    Page { spec: String },
    Offset,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DocumentAddressResolution {
    Found { spans: Vec<ScalarRange> },
    Invalid,
    NoPages,
    NotFound,
    Unavailable,
    Ambiguous,
    NotAddressable,
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
        match label {
            Cow::Borrowed(label) => public_structure_label(label),
            Cow::Owned(label) => Cow::Owned(public_structure_label(&label).into_owned()),
        }
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
            public_structure_label(label)
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
            .map(|(index, position)| (position_label(document, position), index))
            .collect::<HashMap<_, _>>();
        for index in 0..positions.len() {
            let owner = owners
                .get::<str>(position_label(document, &positions[index]).as_ref())
                .copied()
                .unwrap_or(index);
            let mut seen = HashSet::new();
            let parent = std::iter::successors(positions[owner].parent, |&parent| {
                let label = public_structure_label(document.nodes[parent].label.as_deref()?);
                seen.insert(label.clone())
                    .then(|| {
                        owners
                            .get::<str>(label.as_ref())
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

#[derive(Clone, Serialize)]
pub struct DocumentAnchor<'a> {
    kind: DocumentKind,
    label: Cow<'a, str>,
    start: usize,
    end: usize,
    #[serde(rename = "parentLabel", skip_serializing_if = "Option::is_none")]
    parent_label: Option<Cow<'a, str>>,
    #[serde(rename = "rowSpan", skip_serializing_if = "Option::is_none")]
    row_span: Option<usize>,
    #[serde(rename = "columnSpan", skip_serializing_if = "Option::is_none")]
    column_span: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentViewerSlice<'a> {
    pub start: usize,
    pub end: usize,
    pub text: &'a str,
    pub anchors: Vec<DocumentAnchor<'a>>,
    pub primary: Option<DocumentAnchor<'a>>,
    pub depth: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentViewer<'a> {
    pub slices: Vec<DocumentViewerSlice<'a>>,
    pub truncated: bool,
    pub document_revision: &'a str,
}

pub struct DocumentQuery {
    searched: AtomicBool,
    coordinates: OnceLock<crate::text::ScalarCoordinates>,
    lines: OnceLock<Vec<[usize; 3]>>,
    blocks: OnceLock<Vec<BlockPosition>>,
    search: OnceLock<search::SearchIndex>,
}

impl Default for DocumentQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentQuery {
    pub fn new() -> Self {
        Self {
            searched: AtomicBool::new(false),
            coordinates: OnceLock::new(),
            lines: OnceLock::new(),
            blocks: OnceLock::new(),
            search: OnceLock::new(),
        }
    }

    fn positions<'a>(&'a self, document: &DocumentStructure) -> &'a [BlockPosition] {
        self.blocks.get_or_init(|| projected_positions(document))
    }

    fn text<'a>(&'a self, document: &'a DocumentStructure) -> ScalarText<'a> {
        let value = document.query_text();
        ScalarText::with_coordinates(
            value,
            self.coordinates
                .get_or_init(|| crate::text::ScalarCoordinates::new(value)),
        )
    }

    fn lines(&self, text: &ScalarText<'_>) -> &[[usize; 3]] {
        self.lines.get_or_init(|| text.line_map())
    }

    pub fn text_window<'a>(
        &'a self,
        document: &'a DocumentStructure,
        offset: usize,
        start_char: usize,
        limit: usize,
    ) -> DocumentTextWindow<'a> {
        let scalar = self.text(document);
        self.text_window_with_text(document, &scalar, offset, start_char, limit)
    }

    fn text_window_with_text<'a>(
        &self,
        document: &'a DocumentStructure,
        scalar: &ScalarText<'a>,
        offset: usize,
        start_char: usize,
        limit: usize,
    ) -> DocumentTextWindow<'a> {
        let text = document.query_text();
        let text_length = scalar.utf16_len();
        let lines = self.lines(scalar);
        let empty = |status, total_lines, line_length| DocumentTextWindow {
            status,
            rows: Vec::new(),
            next_offset: None,
            next_start_char: None,
            total_lines,
            document_revision: document.revision.as_str(),
            line_length,
            range_start_line: None,
            range_end_line: None,
        };
        if offset == 0 {
            return empty(DocumentTextWindowStatus::InvalidLine, None, None);
        }
        if offset > lines.len() {
            return empty(
                DocumentTextWindowStatus::InvalidLine,
                Some(lines.len()),
                None,
            );
        }

        let mut line_index = offset - 1;
        let mut line_start = lines[line_index][0];
        let requested_start = line_start;
        let validation_end = lines[line_index][1];
        let line_start_utf16 = scalar.utf16_at_byte(line_start).unwrap();
        let line_length = scalar.utf16_at_byte(validation_end).unwrap() - line_start_utf16;
        if start_char > line_length {
            return empty(
                DocumentTextWindowStatus::InvalidCharacter,
                None,
                Some(line_length),
            );
        }
        let requested_utf16 = line_start_utf16 + start_char;
        let Some(requested_byte) = scalar.byte_at_utf16(requested_utf16) else {
            return empty(
                DocumentTextWindowStatus::SplitCharacter,
                None,
                Some(line_length),
            );
        };

        let mut rows = Vec::new();
        let mut rendered_length = 0;
        let mut line_number = offset;
        let mut last_row = None;
        let mut has_more = false;
        loop {
            let display_end = lines[line_index][1];
            let next_start = lines.get(line_index + 1).map_or(text.len(), |line| line[0]);
            let start = if line_start == requested_start {
                requested_byte
            } else {
                line_start
            };
            if display_end > start {
                if rows.len() >= limit {
                    has_more = true;
                    break;
                }
                let start_utf16 = scalar.utf16_at_byte(start).unwrap();
                let display_end_utf16 = scalar.utf16_at_byte(display_end).unwrap();
                let shown_target = start_utf16
                    .saturating_add(READ_LINE_UTF16_LIMIT)
                    .min(display_end_utf16);
                let shown_byte_end = scalar.byte_at_utf16_ceil(shown_target).unwrap();
                let shown_end = scalar.utf16_at_byte(shown_byte_end).unwrap();
                let shown = &text[start..shown_byte_end];
                let truncated_start = start > line_start;
                let truncated_end = shown_byte_end < display_end;
                let added = (line_number.ilog10() as usize + 1).max(6) + 1 + shown_end
                    - start_utf16
                    + usize::from(truncated_start)
                    + usize::from(truncated_end)
                    + usize::from(!rows.is_empty());
                if !rows.is_empty() && rendered_length + added > READ_OUTPUT_UTF16_BUDGET {
                    has_more = true;
                    break;
                }
                rendered_length += added;
                rows.push(DocumentTextWindowRow {
                    line_number,
                    text: shown,
                    span: [start_utf16, shown_end],
                    truncated_start,
                    truncated_end,
                });
                last_row = Some((
                    line_number,
                    scalar.utf16_at_byte(line_start).unwrap(),
                    shown_end,
                    display_end_utf16,
                    scalar.utf16_at_byte(next_start).unwrap(),
                ));
                if shown_end < display_end_utf16 {
                    has_more = true;
                    break;
                }
            }
            if line_index + 1 == lines.len() {
                break;
            }
            line_index += 1;
            line_start = next_start;
            line_number += 1;
        }

        let (next_offset, next_start_char) = if !has_more {
            (None, None)
        } else if let Some((line, start, shown_end, display_end, next)) = last_row {
            if shown_end < display_end {
                (Some(line), Some(shown_end - start))
            } else if next < text_length {
                (Some(line + 1), Some(0))
            } else {
                (None, None)
            }
        } else {
            (Some(offset), Some(start_char))
        };
        DocumentTextWindow {
            status: DocumentTextWindowStatus::Ready,
            rows,
            next_offset,
            next_start_char,
            total_lines: None,
            document_revision: &document.revision,
            line_length: None,
            range_start_line: None,
            range_end_line: None,
        }
    }

    pub fn text_range_window<'a>(
        &'a self,
        document: &'a DocumentStructure,
        start: usize,
        end: usize,
        offset: Option<usize>,
        limit: usize,
    ) -> DocumentTextWindow<'a> {
        let text = document.query_text();
        let scalar = self.text(document);
        let lines = self.lines(&scalar);
        let invalid = || DocumentTextWindow {
            status: DocumentTextWindowStatus::InvalidRange,
            rows: Vec::new(),
            next_offset: None,
            next_start_char: None,
            total_lines: None,
            document_revision: &document.revision,
            line_length: None,
            range_start_line: None,
            range_end_line: None,
        };
        if start > end || end > scalar.utf16_len() {
            return invalid();
        }
        let (Some(start_byte), Some(end_byte)) =
            (scalar.byte_at_utf16(start), scalar.byte_at_utf16(end))
        else {
            return invalid();
        };
        let line_at = |byte: usize| lines.partition_point(|line| line[0] <= byte).max(1);
        let start_line = line_at(start_byte);
        let end_probe = if end_byte > start_byte {
            text[..end_byte]
                .char_indices()
                .next_back()
                .map_or(start_byte, |(byte, _)| byte)
        } else {
            start_byte
        };
        let end_line = line_at(end_probe);
        let requested_line = offset.unwrap_or(start_line);
        if requested_line < start_line || requested_line > end_line {
            let mut window = invalid();
            window.status = DocumentTextWindowStatus::InvalidLine;
            window.range_start_line = Some(start_line);
            window.range_end_line = Some(end_line);
            return window;
        }
        let line_start = lines[requested_line - 1][0];
        let start_char = if requested_line == start_line {
            start - scalar.utf16_at_byte(line_start).unwrap()
        } else {
            0
        };
        let mut window =
            self.text_window_with_text(document, &scalar, requested_line, start_char, limit);
        window.range_start_line = Some(start_line);
        window.range_end_line = Some(end_line);
        if window.status != DocumentTextWindowStatus::Ready {
            return window;
        }

        let mut clipped = false;
        window.rows.retain_mut(|row| {
            if row.span[0] >= end {
                return false;
            }
            if row.span[1] > end {
                row.text = scalar.slice_utf16(row.span[0]..end).unwrap();
                row.span[1] = end;
                row.truncated_end = true;
                clipped = true;
            }
            true
        });
        let end_line_start = lines[end_line - 1][0];
        let end_char = end - scalar.utf16_at_byte(end_line_start).unwrap();
        if clipped
            || window.next_offset.is_some_and(|next| next > end_line)
            || window.next_offset == Some(end_line)
                && window.next_start_char.is_some_and(|next| next >= end_char)
        {
            window.next_offset = None;
            window.next_start_char = None;
        }
        window
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
        block.row_span = node.row_span;
        block.column_span = node.column_span;
        block
    }

    pub fn anchors<'a>(
        &'a self,
        document: &'a DocumentStructure,
        end: Option<usize>,
    ) -> impl Iterator<Item = DocumentAnchor<'a>> + 'a {
        self.positions(document).iter().filter_map(move |position| {
            let node = &document.nodes[position.node];
            let range = document.query_range(node);
            if end.is_some_and(|end| range.start >= end) {
                return None;
            }
            Some(DocumentAnchor {
                kind: projected_kind(node).expect("projected node kind"),
                label: position_label(document, position),
                start: range.start,
                end: end.map_or(range.end, |end| range.end.min(end)),
                parent_label: parent_label(document, position),
                row_span: node.row_span,
                column_span: node.column_span,
            })
        })
    }

    pub fn viewer<'a>(
        &'a self,
        document: &'a DocumentStructure,
        primary_kind: DocumentKind,
        limit: usize,
    ) -> DocumentViewer<'a> {
        let full_text = document.query_text();
        let scalar = self.text(document);
        let full_end = scalar.utf16_len();
        let text = if limit >= full_end {
            full_text
        } else {
            crate::text::utf16_prefix_ceil(full_text, limit)
        };
        let end = if text.len() == full_text.len() {
            full_end
        } else {
            scalar.utf16_at_byte(text.len()).unwrap_or_default()
        };
        let anchors = self
            .anchors(document, Some(end))
            .filter(|anchor| {
                anchor.start < end
                    && (anchor.kind == DocumentKind::Page || anchor.kind == primary_kind)
            })
            .collect::<Vec<_>>();
        let mut grouped = BTreeMap::<usize, Vec<_>>::new();
        for anchor in anchors {
            grouped.entry(anchor.start).or_default().push(anchor);
        }
        if grouped.is_empty() {
            static PARAGRAPH_BREAK: OnceLock<Regex> = OnceLock::new();
            for found in regex(r"\n[ \t]*\n+", &PARAGRAPH_BREAK).find_iter(text) {
                if let Some(start) = scalar.utf16_at_byte(found.end()) {
                    grouped.entry(start).or_default();
                }
            }
        }
        grouped.entry(0).or_default();
        let mut starts = grouped.keys().copied().collect::<Vec<_>>();
        if starts.last() != Some(&end) {
            starts.push(end);
        }
        let mut slices = Vec::with_capacity(grouped.len());
        for (index, start) in starts.iter().copied().enumerate().take(starts.len() - 1) {
            let mut anchors = grouped.remove(&start).unwrap_or_default();
            let mut primary = None;
            let mut primary_length = 0;
            for (index, anchor) in anchors.iter().enumerate() {
                if anchor.kind == primary_kind {
                    let length = crate::text::utf16_len(&anchor.label);
                    if primary.is_none() || length > primary_length {
                        primary = Some(index);
                        primary_length = length;
                    }
                }
            }
            if primary.is_none() {
                primary = anchors
                    .iter()
                    .position(|anchor| anchor.kind == DocumentKind::Page);
            }
            let primary = primary.map(|index| anchors.remove(index));
            let slice_end = starts[index + 1];
            let slice_text = js_trim(scalar.slice_utf16(start..slice_end).unwrap_or_default());
            if !slice_text.is_empty() || primary.is_some() || !anchors.is_empty() {
                let depth = primary
                    .as_ref()
                    .filter(|anchor| anchor.kind == DocumentKind::Section)
                    .map_or(0, |anchor| {
                        anchor
                            .label
                            .strip_prefix("sec")
                            .unwrap_or(anchor.label.as_ref())
                            .bytes()
                            .filter(|byte| matches!(byte, b'(' | b'.' | b'-'))
                            .count()
                            .min(5)
                    });
                slices.push(DocumentViewerSlice {
                    start,
                    end: slice_end,
                    text: slice_text,
                    anchors,
                    depth,
                    primary,
                });
            }
        }
        DocumentViewer {
            slices,
            truncated: limit < full_end,
            document_revision: &document.revision,
        }
    }

    pub fn table_cells(&self, document: &DocumentStructure) -> Vec<AuthoritativeTableCell> {
        let names = document
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Table)
            .filter_map(|node| Some((node.id.as_str(), node.aliases.as_ref()?.first()?.as_str())))
            .collect::<HashMap<_, _>>();
        let text = self.text(document);
        document
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Cell)
            .filter_map(|node| {
                let (row_id, column) = node.id.rsplit_once("/col:")?;
                let (table_id, row) = row_id.rsplit_once("/row:")?;
                let table = table_id.strip_prefix("table:")?.parse().ok()?;
                let row = row.parse().ok()?;
                let column = column.parse().ok()?;
                let range = document.query_range(node);
                Some(AuthoritativeTableCell {
                    table,
                    table_name: Some(
                        names
                            .get(table_id)
                            .map_or_else(|| format!("Table {table}"), |name| (*name).to_owned()),
                    ),
                    row,
                    column,
                    row_span: node.row_span,
                    column_span: node.column_span,
                    address: Some(
                        node.anchor
                            .clone()
                            .unwrap_or_else(|| format!("R{row}C{column}")),
                    ),
                    display_value: Some(node.display_value.clone().unwrap_or_else(|| {
                        js_trim(text.slice_utf16(range.start..range.end).unwrap_or_default())
                            .to_owned()
                    })),
                    start: range.start,
                    end: range.end,
                })
            })
            .collect()
    }

    fn materialize(
        &self,
        document: &DocumentStructure,
        position: &BlockPosition,
        text: &ScalarText<'_>,
    ) -> MaterializedDocumentBlock {
        let block = self.block(document, position);
        MaterializedDocumentBlock {
            text: js_trim(text.slice_utf16(block.start..block.end).unwrap_or_default()).to_owned(),
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

    fn position_resolves(
        &self,
        document: &DocumentStructure,
        position: &BlockPosition,
        label: &str,
    ) -> bool {
        self.position_matches(document, position, label)
            || document.nodes[position.node]
                .anchor
                .as_deref()
                .is_some_and(|anchor| equal_fold(anchor, label))
    }

    fn unique_position<'a>(
        &'a self,
        document: &DocumentStructure,
        label: &str,
    ) -> Option<&'a BlockPosition> {
        let mut matches = self
            .positions(document)
            .iter()
            .filter(|position| self.position_resolves(document, position, label));
        let found = matches.next()?;
        matches.next().is_none().then_some(found)
    }

    fn subtree_labels(&self, document: &DocumentStructure, seed_label: &str) -> Vec<String> {
        let positions = self.positions(document);
        let position_labels = positions
            .iter()
            .map(|position| position_label(document, position))
            .collect::<Vec<_>>();
        // Reverse lookup historically chose the last duplicate label.
        let positions_by_label = position_labels
            .iter()
            .enumerate()
            .map(|(index, label)| (label.as_ref(), index))
            .collect::<HashMap<_, _>>();
        let mut subtree = Vec::new();
        for (block_index, block) in positions.iter().enumerate() {
            let mut current = Some(block_index);
            let mut seen = HashSet::new();
            while let Some(candidate_index) = current {
                let candidate = &positions[candidate_index];
                let label = &position_labels[candidate_index];
                if !seen.insert(label.as_ref()) {
                    break;
                }
                if label.as_ref() == seed_label {
                    subtree.push(position_label(document, block).into_owned());
                    break;
                }
                current = parent_label(document, candidate)
                    .as_deref()
                    .and_then(|parent| positions_by_label.get(parent).copied());
            }
        }
        subtree
    }

    pub fn has_origin(&self, document: &DocumentStructure, origin: DocumentOrigin) -> bool {
        document.nodes.iter().any(|node| {
            projected_kind(node).is_some()
                && (node.kind == NodeKind::Prose || node.label.is_some())
                && (node.source == Derivation::Native) == (origin == DocumentOrigin::Native)
        })
    }

    fn requested_label<'a>(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        locator: &'a str,
    ) -> Cow<'a, str> {
        let exact = js_trim(locator);
        if self
            .positions(document)
            .iter()
            .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind))
            .any(|position| equal_fold(&position_label(document, position), exact))
        {
            return Cow::Borrowed(exact);
        }
        let normalized = normalize_document_locator(kind, locator);
        if !normalized.is_empty() {
            return Cow::Owned(normalized);
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
            .then_some(Cow::Borrowed(exact))
            .unwrap_or_default()
    }

    fn lookup_label(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        requested_label: &str,
        context_blocks: usize,
    ) -> DocumentLookup {
        let empty = |status| DocumentLookup {
            status,
            requested_label: requested_label.to_owned(),
            matches: Vec::new(),
            block: None,
            before: Vec::new(),
            after: Vec::new(),
        };
        if requested_label.is_empty() {
            return empty(DocumentLookupStatus::Unavailable);
        }
        let mut available = Vec::new();
        let mut selected = None;
        let mut ambiguous = false;
        for position in self.positions(document) {
            if projected_kind(&document.nodes[position.node]) == Some(kind) {
                available.push(position);
            }
            if self.position_resolves(document, position, requested_label) {
                ambiguous |= selected.replace(position).is_some();
            }
        }
        if available.is_empty() {
            return empty(DocumentLookupStatus::Unavailable);
        }
        let selected = (if ambiguous { None } else { selected })
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
        let text = self.text(document);
        DocumentLookup {
            status: DocumentLookupStatus::Found,
            requested_label: requested_label.to_owned(),
            matches: vec![position_label(document, selected).into_owned()],
            block: Some(self.materialize(document, selected, &text)),
            before: available[order.saturating_sub(context)..order]
                .iter()
                .map(|position| self.materialize(document, position, &text))
                .collect(),
            after: available[order + 1..(order + 1 + context).min(available.len())]
                .iter()
                .map(|position| self.materialize(document, position, &text))
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
        let available = self
            .positions(document)
            .iter()
            .filter(|position| projected_kind(&document.nodes[position.node]) == Some(kind))
            .collect::<Vec<_>>();
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
        let text = self.text(document);
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
        let mut maximum_end = 0;
        for (_, end) in &mut ranges {
            maximum_end = maximum_end.max(*end);
            *end = maximum_end;
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
                count > 0 && ranges[count - 1].1 >= range.end
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
            .filter_map(|position| {
                let range = document.query_range(&document.nodes[position.node]);
                (range.start <= start && range.end >= end)
                    .then_some((position, range.end - range.start))
            })
            .min_by_key(|(_, length)| *length)?
            .0;
        Some(self.materialize(document, position, &self.text(document)))
    }

    fn page_map(&self, document: &DocumentStructure) -> Vec<PageSpan> {
        let mut pages = Vec::new();
        for position in self.positions(document).iter().filter(|position| {
            projected_kind(&document.nodes[position.node]) == Some(DocumentKind::Page)
        }) {
            let node = &document.nodes[position.node];
            let range = document.query_range(node);
            let page_number = |value: &str, prefix: &str| {
                value
                    .strip_prefix(prefix)
                    .filter(|value| {
                        !value.is_empty()
                            && value.len() <= 6
                            && value.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    .and_then(|value| value.parse().ok())
            };
            let pdf_page = node
                .anchor
                .as_deref()
                .and_then(|anchor| page_number(anchor, "page="))
                .or_else(|| node.page_indexes.first().map(|page| *page + 1))
                .or_else(|| {
                    node.label
                        .as_deref()
                        .and_then(|label| page_number(label, "page"))
                });
            let pdf_page_label = pdf_page.map(|page| page.to_string());
            let printed_label = node
                .aliases
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|alias| js_trim(alias))
                .find(|alias| {
                    !alias.is_empty() && *alias != pdf_page_label.as_deref().unwrap_or("null")
                })
                .map(str::to_owned);
            pages.push(PageSpan {
                pdf_page,
                printed_label,
                start: range.start,
                end: range.end,
            });
        }
        pages.sort_by_key(|page| page.start);
        if pages.is_empty() {
            return page_map_from_markers_with_scalar(document.query_text(), || {
                self.text(document)
            });
        }
        pages
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
        let wanted = js_trim(seed_label);
        let seed = self
            .positions(document)
            .iter()
            .find(|position| equal_fold(&position_label(document, position), wanted))?;
        let seed_label = position_label(document, seed).into_owned();
        let limit = depth.min(3);
        let initial = if include_descendants {
            self.subtree_labels(document, &seed_label)
        } else {
            vec![seed_label.clone()]
        };
        // Graph resolution historically chose the first duplicate label.
        let positions_by_label = self
            .positions(document)
            .iter()
            .rev()
            .map(|position| (position_label(document, position), position))
            .collect::<HashMap<_, _>>();
        let mut reached = vec![*seed];
        let mut reached_labels = initial.iter().cloned().collect::<HashSet<_>>();
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
                if reached_labels.contains(other) {
                    continue;
                }
                let Some(&position) = positions_by_label.get(other) else {
                    continue;
                };
                reached.push(*position);
                reached_labels.insert(other.to_owned());
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
        let text = self.text(document);
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

    pub fn resolve_address_spans(
        &self,
        document: &DocumentStructure,
        spec: &str,
        follow: FollowDirection,
        depth: usize,
    ) -> DocumentAddressResolution {
        match parse_address(spec) {
            None | Some(DocumentAddress::Offset) => DocumentAddressResolution::Invalid,
            Some(DocumentAddress::Page { spec }) => {
                match resolve_page_in_map(&self.page_map(document), &spec) {
                    PageLookup::Found(range) => {
                        DocumentAddressResolution::Found { spans: vec![range] }
                    }
                    PageLookup::NoPages => DocumentAddressResolution::NoPages,
                    PageLookup::NotFound => DocumentAddressResolution::NotFound,
                }
            }
            Some(DocumentAddress::Section { locator }) => {
                let lookup = self.structure_block(document, &locator, 0);
                let block = match (lookup.status, lookup.block) {
                    (DocumentLookupStatus::Found, Some(block)) => block,
                    (DocumentLookupStatus::Unavailable, _) => {
                        return DocumentAddressResolution::Unavailable
                    }
                    (DocumentLookupStatus::Ambiguous, _) => {
                        return DocumentAddressResolution::Ambiguous
                    }
                    _ => return DocumentAddressResolution::NotFound,
                };
                let mut spans = vec![ScalarRange {
                    start: block.block.start,
                    end: block.block.end,
                }];
                if follow != FollowDirection::None {
                    let Some(graph) = document
                        .cross_references
                        .as_ref()
                        .filter(|graph| !graph.document_abstained)
                    else {
                        return DocumentAddressResolution::NotAddressable;
                    };
                    let Some(scope) = self.graph_scope(
                        document,
                        graph,
                        &block.block.label,
                        follow,
                        depth,
                        false,
                        false,
                    ) else {
                        return DocumentAddressResolution::NotAddressable;
                    };
                    spans.extend(scope.nodes.into_iter().map(|node| ScalarRange {
                        start: node.block.block.start,
                        end: node.block.block.end,
                    }));
                }
                DocumentAddressResolution::Found { spans }
            }
        }
    }
}

pub fn normalize_document_locator(kind: DocumentKind, locator: &str) -> String {
    static FOOTNOTE: OnceLock<Regex> = OnceLock::new();
    static PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    static PAGE: OnceLock<Regex> = OnceLock::new();
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
        DocumentKind::Section => return normalize_section_locator(locator),
        _ => return String::new(),
    }
}

fn page_map_from_markers_with_scalar<'a>(
    text: &str,
    scalar: impl FnOnce() -> ScalarText<'a>,
) -> Vec<PageSpan> {
    let mut byte = 0;
    let mut markers = text.split_inclusive('\n').filter_map(|line| {
        let start = byte;
        byte += line.len();
        let label = literal_page_marker(line, false)?;
        (!label.is_empty() && label.len() <= 40 && !label.contains(']')).then_some((start, label))
    });
    let Some(first) = markers.next() else {
        return Vec::new();
    };
    let scalar = scalar();
    let mut pages = Vec::<PageSpan>::new();
    for (start_byte, raw_label) in std::iter::once(first).chain(markers) {
        let label = js_trim(raw_label);
        if label.is_empty() {
            continue;
        }
        let start = scalar.utf16_at_byte(start_byte).expect("marker boundary");
        if let Some(previous) = pages.last_mut() {
            previous.end = start;
        }
        let pdf_page = (label.len() <= 6 && label.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| label.parse().ok())
            .flatten();
        pages.push(PageSpan {
            pdf_page,
            printed_label: Some(label.to_owned()),
            start,
            end: scalar.utf16_len(),
        });
    }
    pages
}

fn resolve_page_in_map(pages: &[PageSpan], requested: &str) -> PageLookup {
    static QUALIFIED: OnceLock<Regex> = OnceLock::new();
    if pages.is_empty() {
        return PageLookup::NoPages;
    }
    let raw = js_trim(requested);
    let qualified = regex_parts(
        &[r"(?iu)^(pdf|printed)", JS_WS, r"*[:=]", JS_WS, r"*(.+)$"],
        &QUALIFIED,
    )
    .captures(raw);
    let wanted = js_trim(qualified.as_ref().map_or(raw, |capture| &capture[2]));
    let pdf = qualified.as_ref().map_or_else(
        || {
            !wanted.is_empty()
                && wanted.len() <= 6
                && wanted.bytes().all(|byte| byte.is_ascii_digit())
        },
        |capture| capture[1].eq_ignore_ascii_case("pdf"),
    );
    let wanted_pdf = if pdf {
        if wanted == "null" {
            Some(None)
        } else {
            wanted
                .parse::<usize>()
                .ok()
                .filter(|value| value.to_string() == wanted)
                .map(Some)
        }
    } else {
        None
    };
    let page = pages.iter().find(|page| {
        if pdf {
            wanted_pdf == Some(page.pdf_page)
        } else {
            page.printed_label
                .as_deref()
                .is_some_and(|label| equal_fold(label, wanted))
        }
    });
    if let Some(page) = page {
        return PageLookup::Found(ScalarRange {
            start: page.start,
            end: page.end,
        });
    }
    PageLookup::NotFound
}

fn parse_address(spec: &str) -> Option<DocumentAddress> {
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
    if regex_parts(
        &[
            r"(?iu)^(?:off|offset)",
            JS_WS,
            r"*[:.]?",
            JS_WS,
            r"*(\d{1,9})$",
        ],
        &OFFSET,
    )
    .is_match(raw)
    {
        return Some(DocumentAddress::Offset);
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
    fn query_uses_one_rendered_plane_and_exact_repeat_search() {
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
        let text = query.text(&document);
        for spans in [
            query.phrase_spans_with_text(&document, &words, PhraseOptions::default(), &text),
            query.phrase_spans_with_text(&document, &words, PhraseOptions::default(), &text),
        ] {
            assert_eq!(spans.len(), 1);
            assert_eq!((spans[0].start, spans[0].end), (0, 10));
        }
        assert!(query
            .phrase_spans_with_text(
                &document,
                &["Clean".to_owned(), "text".to_owned()],
                PhraseOptions::default(),
                &text,
            )
            .is_empty());
        let json = serde_json::to_value(&document).unwrap();
        assert_eq!(json["text"], "raw");
        assert_eq!(json["rendered_text"], "Clean text");
        assert_eq!(json["nodes"][0]["rendered_range"]["start"], 0);
        assert_eq!(json["nodes"][0]["rendered_range"]["end"], 10);
    }
}
