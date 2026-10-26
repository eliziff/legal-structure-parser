use crate::{
    locator::{literal_page_marker, normalize_section_locator},
    public_structure_label,
    text::{equal_fold, trim_javascript_whitespace as js_trim, JS_WHITESPACE_CLASS as JS_WS},
    AuthoritativeTableCell, Derivation, DetectionProfile, DocumentBlock, DocumentKind,
    DocumentOrigin, DocumentStructure, InstrumentCrossReferenceGraph,
    InstrumentCrossReferenceStatus, NodeKind, ScalarRange, ScalarText, StructureNode,
};
use legal_structure_model::InstrumentCrossReferenceEdge;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{atomic::AtomicBool, OnceLock};

mod search;
mod source_text;
mod text_fragment;
pub use source_text::TextStatistics;
use search::{quote_text, PhraseOptions, PhraseSpan};
pub use search::{tokenize_source_text, DocumentWordSpan};
pub use text_fragment::{
    sentence_end, source_text_fragment_plan, text_fragment_plan, text_fragment_plan_in,
    TextFragmentPage, TextFragmentPlan,
    TextFragmentWordInterval,
};

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

/// Read exact membership with the manuscript's own seams; omitted content stays separated.
fn context(value: usize) -> usize {
    value.min(2)
}

/// Authorities-lite's bounded numbered-item lookup, using the source's line
/// starts in the same offset unit as the containing paragraph. A repeated item
/// is unresolved; the next visible marker bounds a unique item's extent.
pub fn numbered_item_range(
    parent: ScalarRange,
    lines: &[(usize, String)],
    item: &str,
) -> Option<ScalarRange> {
    static MARKER: OnceLock<Regex> = OnceLock::new();
    let marker = js_regex(
        r"^\s*(?:\(([0-9]+[a-zA-Z]?)\)|([0-9]+[a-zA-Z]?)[.)])\s+",
        &MARKER,
    );
    let markers = lines
        .iter()
        .filter(|(start, _)| *start >= parent.start && *start < parent.end)
        .filter_map(|(start, text)| {
            marker.captures(text).map(|captures| {
                (
                    *start,
                    captures
                        .get(1)
                        .or_else(|| captures.get(2))
                        .unwrap()
                        .as_str(),
                )
            })
        })
        .collect::<Vec<_>>();
    let mut matches = markers
        .iter()
        .enumerate()
        .filter(|(_, (_, label))| *label == item);
    let (index, (start, _)) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(ScalarRange {
        start: *start,
        end: markers
            .get(index + 1)
            .map_or(parent.end, |(start, _)| *start),
    })
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
    pub start_char: usize,
    pub end_char: usize,
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
    printed_labels: Vec<String>,
    ranges: Vec<ScalarRange>,
    start: usize,
    end: usize,
}

enum PageLookup {
    Found(Vec<ScalarRange>),
    Ambiguous,
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

/// A caller locator or an already-resolved source node; these namespaces stay distinct.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GraphScopeSeed {
    Locator(String),
    NodeId(String),
}

#[derive(Serialize)]
pub struct GraphScope {
    pub seed: MaterializedDocumentBlock,
    pub nodes: Vec<GraphScopeNode>,
    pub depth: usize,
    /// Relevant edges omitted from traversal, separate from returned-text coverage.
    /// Edges need a mapped endpoint on the requested traversal side to be relevant.
    /// In particular, an inbound edge with no mapped target cannot be attributed to this scope.
    pub omissions: Vec<GraphScopeOmission>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphScopeOmission {
    /// Index in the document graph, reported once even when encountered on multiple hops.
    pub edge_index: usize,
    pub edge: InstrumentCrossReferenceEdge,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_node_id: Option<String>,
    pub missing_endpoints: Vec<&'static str>,
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
        NodeKind::Heading
        | NodeKind::Endnote
        | NodeKind::List
        | NodeKind::ListItem
        | NodeKind::Quote => return None,
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
            let prose_number = if node.kind == NodeKind::Prose && node.label.is_none() {
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
    #[serde(rename = "nodeId")]
    node_id: &'a str,
    #[serde(rename = "printedLabel")]
    printed_label: bool,
    ranges: Vec<ScalarRange>,
    level: usize,
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
    /// The block's function where its kind does not say it: a headnote, a caption, the bench,
    /// counsel, a date, a docket number, the court.
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<legal_structure_model::PrimaryRole>,
}

impl DocumentAnchor<'_> {
    pub fn kind(&self) -> DocumentKind { self.kind }
    pub fn label(&self) -> &str { &self.label }
    /// Its range in the document's text, in characters.
    pub fn range(&self) -> (usize, usize) { (self.start, self.end) }
    pub fn parent_label(&self) -> Option<&str> { self.parent_label.as_deref() }
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
    /// The slice is a heading the structure reads, not a block of text.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub heading: bool,
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
    replay: OnceLock<text_fragment::ReplayIndex>,
    normalized_fragment_text: OnceLock<String>,
    /// The page a reader shows of the document's own text (A2AJ Law Web's), indexed once.
    source_page: OnceLock<TextFragmentPage>,
    /// Where the names of legal references end in the document's text (UTF-16, sorted).
    legal_reference_ends: OnceLock<Vec<usize>>,
}

impl Default for DocumentQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentQuery {
    /// The page a reader shows of the document's own text, made by `page` on the first call: one document is shown
    /// by one reader.
    pub fn source_page(&self, page: impl FnOnce() -> String) -> &TextFragmentPage {
        self.source_page.get_or_init(|| TextFragmentPage::new(page()))
    }

    /// Reads where the names of legal references a publisher links end in the document's text, with `ends` (byte
    /// offsets in that text) on the first call: a plan for a page that links them breaks its terms there.
    pub fn read_legal_reference_ends(&self, document: &DocumentStructure, ends: impl FnOnce(&str) -> Vec<usize>) {
        self.legal_reference_ends.get_or_init(|| {
            let text = self.text(document);
            let mut bytes = ends(text.value);
            bytes.sort_unstable();
            let (mut units, mut at) = (0, 0);
            bytes.into_iter().filter(|&byte| text.value.is_char_boundary(byte)).map(|byte| {
                units += crate::utf16_len(&text.value[at..byte]);
                at = byte;
                units
            }).collect()
        });
    }

    pub fn new() -> Self {
        Self {
            searched: AtomicBool::new(false),
            coordinates: OnceLock::new(),
            lines: OnceLock::new(),
            blocks: OnceLock::new(),
            search: OnceLock::new(),
            replay: OnceLock::new(),
            normalized_fragment_text: OnceLock::new(),
            source_page: OnceLock::new(),
            legal_reference_ends: OnceLock::new(),
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
                    start_char: start_utf16 - scalar.utf16_at_byte(line_start).unwrap(),
                    end_char: shown_end - scalar.utf16_at_byte(line_start).unwrap(),
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
        start_char: Option<usize>,
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
        let start_char = start_char.unwrap_or(0).max(if requested_line == start_line {
            start - scalar.utf16_at_byte(line_start).unwrap()
        } else {
            0
        });
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
                row.end_char -= row.span[1] - end;
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
        block.node_id = Some(node.id.clone());
        block.printed_label = node.label.is_some()
            && node.marker_range.is_some_and(|range| range.start < range.end);
        block.ranges = document.query_ranges(node).into_owned();
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
                node_id: &node.id,
                printed_label: node.label.is_some()
                    && node.marker_range.is_some_and(|range| range.start < range.end),
                ranges: document.query_ranges(node).iter().filter_map(|range| {
                    let end = end.map_or(range.end, |end| range.end.min(end));
                    (range.start <= end).then_some(ScalarRange { start: range.start, end })
                }).collect(),
                level: node.level.unwrap_or_else(|| {
                    let mut level = 1;
                    let mut parent = position.parent;
                    let mut seen = HashSet::new();
                    while let Some(index) = parent.filter(|index| seen.insert(*index)) {
                        level += 1;
                        parent = document.nodes[index].parent_id.as_ref()
                            .and_then(|id| document.nodes.iter().position(|node| &node.id == id));
                    }
                    level
                }),
                kind: projected_kind(node).expect("projected node kind"),
                label: position_label(document, position),
                start: range.start,
                end: end.map_or(range.end, |end| range.end.min(end)),
                parent_label: parent_label(document, position),
                row_span: node.row_span,
                column_span: node.column_span,
                role: node.role.filter(|role| !matches!(role,
                    legal_structure_model::PrimaryRole::Text | legal_structure_model::PrimaryRole::Heading)),
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
        let mut breaks = Vec::new();
        for anchor in anchors {
            if anchor.kind == primary_kind {
                breaks.push(anchor.end);
            }
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
        // A heading, and text after a block that no block holds, read apart from the block before.
        let headings = document
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Heading)
            .map(|node| document.query_range(node).start)
            .filter(|start| *start < end)
            .collect::<HashSet<_>>();
        for at in breaks.into_iter().chain(headings.iter().copied()) {
            if at < end {
                grouped.entry(at).or_default();
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
                    .map_or(0, |anchor| anchor.level.saturating_sub(1));
                slices.push(DocumentViewerSlice {
                    start,
                    end: slice_end,
                    text: slice_text,
                    anchors,
                    depth,
                    heading: primary.is_none() && headings.contains(&start),
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
        let nodes = document.nodes.iter().map(|node| (node.id.as_str(), node)).collect::<HashMap<_, _>>();
        let text = self.text(document);
        document
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Cell)
            .filter_map(|node| {
                let row_node = nodes.get(node.parent_id.as_deref()?)?;
                if row_node.kind != NodeKind::Row { return None; }
                let table_node = nodes.get(row_node.parent_id.as_deref()?)?;
                if table_node.kind != NodeKind::Table { return None; }
                let table = table_node.table_number?;
                let row = node.row?;
                let column = node.column?;
                let range = document.query_range(node);
                Some(AuthoritativeTableCell {
                    ranges: document.query_ranges(node).into_owned(),
                    table,
                    table_name: Some(
                        table_node.aliases.as_ref().and_then(|names| names.first())
                            .cloned().unwrap_or_else(|| format!("Table {table}")),
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
                        crate::text::membership_text_utf16(&text, &document.query_ranges(node))
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
            text: crate::text::membership_text_utf16(&text, &block.ranges),
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
        let node = &document.nodes[position.node];
        let matches_label =
            if node.kind == NodeKind::Prose && node.label.is_some() && node.label == node.anchor {
                position_label(document, position) == label
            } else {
                equal_fold(&position_label(document, position), label)
            };
        let matches = matches_label
            || node
                .aliases
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|alias| equal_fold(alias, label));
        // Automatic prose numbering must not obscure a paragraph label that
        // the document actually declares. Keep duplicate declared paragraphs
        // ambiguous and retain every prose block in the ordinary projection.
        matches
            && (position.prose.is_none()
                || !self.positions(document).iter().any(|candidate| {
                    candidate.prose.is_none()
                        && document.nodes[candidate.node].kind == NodeKind::Paragraph
                        && self.position_matches(document, candidate, label)
                }))
    }

    fn position_resolves(
        &self,
        document: &DocumentStructure,
        position: &BlockPosition,
        label: &str,
        kind: Option<DocumentKind>,
    ) -> bool {
        self.position_matches(document, position, label)
            || (document.nodes[position.node]
                .anchor
                .as_deref()
                .is_some_and(|anchor| anchor == label)
                // Publisher IDs and legal labels are separate namespaces: a
                // displayed footnote 1 can have browser ID fn0, while fn1 is
                // another footnote's ID. A known legal label in the requested
                // kind takes precedence; untyped graph lookup considers all kinds.
                && !self.positions(document).iter().any(|candidate| {
                    kind.is_none_or(|kind| projected_kind(&document.nodes[candidate.node]) == Some(kind))
                        && self.position_matches(document, candidate, label)
                }))
    }

    fn unique_position<'a>(
        &'a self,
        document: &DocumentStructure,
        label: &str,
        kind: Option<DocumentKind>,
    ) -> Option<&'a BlockPosition> {
        let mut matches = self
            .positions(document)
            .iter()
            .filter(|position| kind.is_none_or(|kind| projected_kind(&document.nodes[position.node]) == Some(kind)))
            .filter(|position| self.position_resolves(document, position, label, kind));
        let found = matches.next()?;
        matches.next().is_none().then_some(found)
    }

    fn subtree_positions<'a>(
        &'a self,
        document: &DocumentStructure,
        seed: &BlockPosition,
    ) -> Vec<&'a BlockPosition> {
        let by_id = document.nodes.iter().enumerate()
            .map(|(index, node)| (node.id.as_str(), index)).collect::<HashMap<_, _>>();
        self.positions(document).iter().filter(|position| {
            let mut current = Some(position.node);
            let mut seen = HashSet::new();
            while let Some(index) = current {
                if !seen.insert(index) { break; }
                if index == seed.node { return true; }
                current = document.nodes[index].parent_id.as_deref()
                    .and_then(|parent| by_id.get(parent).copied());
            }
            false
        }).collect()
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
            if projected_kind(&document.nodes[position.node]) != Some(kind) {
                continue;
            }
            available.push(position);
            if self.position_resolves(document, position, requested_label, Some(kind)) {
                ambiguous |= selected.replace(position).is_some();
            }
        }
        if available.is_empty() {
            return empty(DocumentLookupStatus::Unavailable);
        }
        let selected = if ambiguous { None } else { selected };
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
                .unique_position(document, &label, Some(kind))?;
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
        blocks: &[&'a BlockPosition],
    ) -> Vec<&'a BlockPosition> {
        let Some(kind) = blocks
            .first()
            .and_then(|position| projected_kind(&document.nodes[position.node]))
        else {
            return Vec::new();
        };
        // Parallel reporter pages may overlap without owning one another.
        if kind == DocumentKind::Page {
            return blocks.to_vec();
        }
        let mut ranges = blocks
            .iter()
            .flat_map(|position| document.query_ranges(&document.nodes[position.node]).into_owned())
            .map(|range| (range.start, range.end))
            .collect::<Vec<_>>();
        ranges.sort_unstable();
        let mut covered = Vec::<(usize, usize)>::new();
        for (start, end) in ranges {
            if let Some(previous) = covered.last_mut().filter(|previous| start <= previous.1) {
                previous.1 = previous.1.max(end);
            } else {
                covered.push((start, end));
            }
        }
        let ranges = covered;
        let contained = self
            .positions(document)
            .iter()
            .filter(|position| {
                if projected_kind(&document.nodes[position.node]) != Some(kind) {
                    return false;
                }
                let range = document.query_range(&document.nodes[position.node]);
                // A point at a paragraph boundary is not contained prose from
                // the adjacent paragraph. Keep it when explicitly selected.
                if range.start == range.end {
                    return blocks.iter().any(|block| block.node == position.node);
                }
                document.query_ranges(&document.nodes[position.node]).iter().all(|part| {
                    let count = ranges.partition_point(|(start, _)| *start <= part.start);
                    count > 0 && ranges[count - 1].1 >= part.end
                })
            })
            .collect::<Vec<_>>();
        if kind != DocumentKind::Section || contained.len() <= 1 {
            return contained;
        }
        // Range membership determines eligibility; only source parent edges
        // determine ancestry, including through unprojected intermediate nodes.
        let by_id = document.nodes.iter().enumerate()
            .map(|(index, node)| (node.id.as_str(), index)).collect::<HashMap<_, _>>();
        let eligible = contained.iter().map(|position| position.node).collect::<HashSet<_>>();
        let mut parents = HashSet::new();
        for candidate in &contained {
            let mut current = document.nodes[candidate.node].parent_id.as_deref()
                .and_then(|parent| by_id.get(parent).copied());
            let mut seen = HashSet::from([candidate.node]);
            while let Some(index) = current {
                if !seen.insert(index) { break; }
                if eligible.contains(&index) { parents.insert(index); }
                current = document.nodes[index].parent_id.as_deref()
                    .and_then(|parent| by_id.get(parent).copied());
            }
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
            .filter(|unit| !unit.text.is_empty() || unit.block.anchor.is_some())
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
                let ranges = document.query_ranges(&document.nodes[position.node]);
                ranges.iter().any(|range| range.start <= start && range.end >= end)
                    .then_some((position, ranges.iter().map(|range| range.end - range.start).sum::<usize>()))
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
                .or_else(|| node.page_indexes.first().map(|page| *page + 1));
            let printed_labels = node
                .aliases
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|alias| js_trim(alias))
                .filter(|alias| !alias.is_empty())
                .map(str::to_owned)
                .collect();
            pages.push(PageSpan {
                pdf_page,
                printed_labels,
                ranges: document.query_ranges(node).into_owned(),
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

    /// Resolve an existing node identity without crossing display-label namespaces.
    pub fn structure_selection(
        &self,
        document: &DocumentStructure,
        selector: &GraphScopeSeed,
        context_blocks: usize,
    ) -> DocumentLookup {
        let GraphScopeSeed::NodeId(id) = selector else {
            let GraphScopeSeed::Locator(locator) = selector else { unreachable!() };
            return self.structure_block(document, locator, context_blocks);
        };
        let positions = self.positions(document);
        let selected = positions.iter().find(|position| document.nodes[position.node].id == *id);
        let Some(selected) = selected else {
            return DocumentLookup { status: DocumentLookupStatus::NotFound,
                requested_label: id.clone(), matches: Vec::new(), block: None,
                before: Vec::new(), after: Vec::new() };
        };
        let kind = projected_kind(&document.nodes[selected.node]);
        let available = positions.iter().filter(|position|
            projected_kind(&document.nodes[position.node]) == kind).collect::<Vec<_>>();
        let index = available.iter().position(|position| position.node == selected.node).unwrap();
        let context = context(context_blocks);
        let text = self.text(document);
        DocumentLookup {
            status: DocumentLookupStatus::Found,
            requested_label: id.clone(),
            matches: vec![position_label(document, selected).into_owned()],
            block: Some(self.materialize(document, selected, &text)),
            before: available[index.saturating_sub(context)..index].iter()
                .map(|position| self.materialize(document, position, &text)).collect(),
            after: available[index + 1..(index + 1 + context).min(available.len())].iter()
                .map(|position| self.materialize(document, position, &text)).collect(),
        }
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
        seed: &GraphScopeSeed,
        follow: FollowDirection,
        depth: usize,
        include_descendants: bool,
        include_units: bool,
    ) -> Option<GraphScope> {
        let seed = match seed {
            GraphScopeSeed::Locator(locator) => self.unique_position(document, js_trim(locator), None)?,
            GraphScopeSeed::NodeId(id) => self.positions(document).iter()
                .find(|position| document.nodes[position.node].id == *id)?,
        };
        self.graph_scope_position(document, graph, seed, follow, depth, include_descendants, include_units)
    }

    fn graph_scope_position(
        &self,
        document: &DocumentStructure,
        graph: &InstrumentCrossReferenceGraph,
        seed: &BlockPosition,
        follow: FollowDirection,
        depth: usize,
        include_descendants: bool,
        include_units: bool,
    ) -> Option<GraphScope> {
        let limit = depth.min(3);
        let initial = if include_descendants {
            self.subtree_positions(document, seed)
        } else {
            vec![seed]
        };
        // Labels are display locators, not node identities. Edge coordinates must
        // identify exactly one reflected owner, including across repeated labels.
        let endpoint = |label: &str, start: usize, end: usize, target: bool| {
            let mut matches = self.positions(document).iter().filter(|position| {
                if projected_kind(&document.nodes[position.node]) != Some(DocumentKind::Section)
                    || !equal_fold(&position_label(document, position), label) { return false; }
                let node = &document.nodes[position.node];
                if target {
                    // The graph producer records the source node's range, not its
                    // rendered projection or descendant extent.
                    node.range.start == start && node.range.end == end
                } else {
                    let ranges = if !node.extent.is_empty() { &node.extent[..] }
                        else if !node.ranges.is_empty() { &node.ranges[..] }
                        else { std::slice::from_ref(&node.range) };
                    // The producer assigns a citation by its opening source anchor;
                    // its wording can continue across distinct owned fragments.
                    ranges.iter().any(|range| range.start <= start && start < range.end)
                }
            });
            let found = matches.next()?;
            matches.next().is_none().then_some(found)
        };
        let mut reached = vec![*seed];
        let mut reached_nodes = initial.iter().map(|position| position.node).collect::<HashSet<_>>();
        let mut frontier = reached_nodes.clone();
        let mut hops = 0;
        let mut omissions = Vec::new();
        let mut omitted_edges = HashSet::new();
        while follow != FollowDirection::None && hops < limit && !frontier.is_empty() {
            let mut next = HashSet::new();
            for (edge_index, edge) in graph.edges.iter().enumerate() {
                if edge.self_loop { continue; }
                let source = edge.source_label.as_deref()
                    .and_then(|label| endpoint(label, edge.source_start, edge.source_end, false));
                let target = edge.target_label.as_deref().zip(edge.target_start.zip(edge.target_end))
                    .and_then(|(label, (start, end))| endpoint(label, start, end, true));
                let forward = matches!(follow, FollowDirection::Out | FollowDirection::Both)
                    && source.is_some_and(|source| frontier.contains(&source.node));
                let backward = matches!(follow, FollowDirection::In | FollowDirection::Both)
                    && target.is_some_and(|target| frontier.contains(&target.node));
                if !forward && !backward { continue; }
                if edge.status != InstrumentCrossReferenceStatus::Resolved || source.is_none() || target.is_none() {
                    if omitted_edges.insert(edge_index) {
                        omissions.push(GraphScopeOmission {
                            edge_index,
                            edge: edge.clone(),
                            source_node_id: source.map(|source| document.nodes[source.node].id.clone()),
                            target_node_id: target.map(|target| document.nodes[target.node].id.clone()),
                            missing_endpoints: [("source", source.is_none()), ("target", target.is_none())]
                                .into_iter().filter_map(|(endpoint, missing)| missing.then_some(endpoint)).collect(),
                        });
                    }
                    continue;
                }
                let (Some(source), Some(target)) = (source, target) else { unreachable!() };
                let other = if forward { target } else { source };
                if reached_nodes.insert(other.node) {
                    reached.push(*other);
                    next.insert(other.node);
                }
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
            omissions,
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
                    PageLookup::Found(spans) => {
                        DocumentAddressResolution::Found { spans }
                    }
                    PageLookup::NoPages => DocumentAddressResolution::NoPages,
                    PageLookup::NotFound => DocumentAddressResolution::NotFound,
                    PageLookup::Ambiguous => DocumentAddressResolution::Ambiguous,
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
                let mut spans = block.block.ranges.clone();
                if follow != FollowDirection::None {
                    let Some(graph) = document
                        .cross_references
                        .as_ref()
                        .filter(|graph| !graph.document_abstained)
                    else {
                        return DocumentAddressResolution::NotAddressable;
                    };
                    let Some(seed) = self.positions(document).iter().find(|position|
                        block.block.node_id.as_deref() == Some(document.nodes[position.node].id.as_str()))
                    else { return DocumentAddressResolution::NotAddressable; };
                    let Some(scope) = self.graph_scope_position(
                        document,
                        graph,
                        seed,
                        follow,
                        depth,
                        false,
                        false,
                    ) else {
                        return DocumentAddressResolution::NotAddressable;
                    };
                    spans.extend(scope.nodes.into_iter().flat_map(|node| node.block.block.ranges));
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
            previous.ranges[0].end = start;
        }
        let pdf_page = (label.len() <= 6 && label.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| label.parse().ok())
            .flatten();
        pages.push(PageSpan {
            pdf_page,
            printed_labels: vec![label.to_owned()],
            ranges: vec![ScalarRange { start, end: scalar.utf16_len() }],
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
    let mut matches = pages.iter().filter(|page| {
        if pdf {
            wanted_pdf == Some(page.pdf_page)
        } else {
            page.printed_labels
                .iter()
                .any(|label| equal_fold(label, wanted))
        }
    });
    if let Some(page) = matches.next() {
        if matches.next().is_some() {
            return PageLookup::Ambiguous;
        }
        return PageLookup::Found(page.ranges.clone());
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
