use legal_locator::{normalize_section_locator, parse_ordinal};
use crate::{
    text::{equal_fold, trim_javascript_whitespace as js_trim, JS_WHITESPACE_CLASS as JS_WS},
    AuthoritativeTableCell, Derivation, DetectionProfile,
    DocumentStructure, InstrumentCrossReferenceGraph,
    InstrumentCrossReferenceStatus, NodeKind, ScalarRange, ScalarText, StructureNode,
};
use legal_structure_model::{provision_mark, InstrumentCrossReferenceEdge};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{atomic::AtomicBool, OnceLock};

mod search;
mod source_text;
mod provisions;
mod text_fragment;
pub use source_text::TextStatistics;
pub use provisions::{note_named, Provisions};
use search::{quote_text, PhraseOptions, PhraseSpan};
pub use search::{lowercase_words, tokenize_source_text, word_regex, DocumentWordSpan};
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

const READ_OUTPUT_UTF16_BUDGET: usize = 63_000;
const READ_LINE_UTF16_LIMIT: usize = READ_OUTPUT_UTF16_BUDGET - 1_000;

/// A node the query found, read (its ranges, span and fragment), and its text as the query reads it: its membership in
/// the document's query text.
#[derive(Clone, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct Passage<'a> {
    #[serde(flatten)]
    pub reading: legal_structure_model::NodeReading<'a>,
    pub text: String,
}

/// What a passage is cited by: a page, paragraph, provision or note by the label it prints, and its link fragment.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct Locator {
    pub kind: NodeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub fragment: Option<String>,
}

/// A table cell a row of the text shows: the part of the row's span it holds, by its table's name and its address.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RowCell {
    pub start: usize,
    pub end: usize,
    pub table_name: String,
    pub address: String,
}

#[derive(Clone, Copy, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DocumentLookupStatus {
    Found,
    NotFound,
    Unavailable,
    Ambiguous,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentLookup<'a> {
    pub status: DocumentLookupStatus,
    pub requested_label: String,
    pub matches: Vec<String>,
    pub block: Option<Passage<'a>>,
    pub before: Vec<Passage<'a>>,
    pub after: Vec<Passage<'a>>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct DocumentRangeLookup<'a> {
    pub selected: Vec<Passage<'a>>,
    pub before: Vec<Passage<'a>>,
    pub after: Vec<Passage<'a>>,
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DocumentTextWindowStatus {
    Ready,
    InvalidLine,
    InvalidCharacter,
    SplitCharacter,
    InvalidRange,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentTextWindowRow<'a> {
    pub line_number: usize,
    pub text: &'a str,
    pub span: [usize; 2],
    pub start_char: usize,
    pub end_char: usize,
    pub truncated_start: bool,
    pub truncated_end: bool,
    /// The smallest block holding the row's span, what the span is cited by (`DocumentQuery::locator`), and the
    /// table cells the row shows.
    pub block: Option<Passage<'a>>,
    pub locator: Option<Locator>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<RowCell>>", optional))]
    pub cells: Vec<RowCell>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentTextWindow<'a> {
    pub status: DocumentTextWindowStatus,
    pub rows: Vec<DocumentTextWindowRow<'a>>,
    pub next_offset: Option<usize>,
    pub next_start_char: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub total_lines: Option<usize>,
    pub document_revision: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub line_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub range_start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub range_end_line: Option<usize>,
}

enum DocumentAddress {
    Section { locator: String },
    Page { spec: String },
    Offset,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum FollowDirection {
    None,
    Out,
    In,
    Both,
}

/// A caller locator or an already-resolved source node; these namespaces stay distinct.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum GraphScopeSeed {
    Locator(String),
    NodeId(String),
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct GraphScope<'a> {
    pub seed: Passage<'a>,
    pub nodes: Vec<GraphScopeNode<'a>>,
    pub depth: usize,
    /// Relevant edges omitted from traversal, separate from returned-text coverage.
    /// Edges need a mapped endpoint on the requested traversal side to be relevant.
    /// In particular, an inbound edge with no mapped target cannot be attributed to this scope.
    pub omissions: Vec<GraphScopeOmission>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct GraphScopeOmission {
    /// Index in the document graph, reported once even when encountered on multiple hops.
    pub edge_index: usize,
    pub edge: InstrumentCrossReferenceEdge,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub source_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub target_node_id: Option<String>,
    pub missing_endpoints: Vec<&'static str>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct GraphScopeNode<'a> {
    #[serde(flatten)]
    pub block: Passage<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub units: Option<Vec<Passage<'a>>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProjectionOrder {
    Case,
    Legislation,
    Position,
    StablePosition,
    Native,
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

/// Whether a node is one the query serves: a paragraph, prose, a page, a provision, a footnote, a table, its row or its
/// cell; a heading, an endnote, a list, its item, a quotation and a span (a reading of the text that is no block of it:
/// a headnote, a front line, a defined term) are none. The structure holds them; the usual calls leave them out.
fn projected(node: &StructureNode) -> bool {
    matches!(node.kind, NodeKind::Prose | NodeKind::Paragraph | NodeKind::Page | NodeKind::Section | NodeKind::Footnote
        | NodeKind::Table | NodeKind::Row | NodeKind::Cell)
}

/// Whether a node is served as `kind`: its own kind, or prose where paragraphs are asked for.
pub(crate) fn queried_as(node: &StructureNode, kind: NodeKind) -> bool {
    node.kind == kind || kind == NodeKind::Paragraph && node.kind == NodeKind::Prose
}

/// Whether a locator names a node by its label: a provision by its place, its kind's mark and its label ("sec5(2)",
/// "part2", as a citation's locator reads: `normalize_section_locator`), any other node by its label.
fn labelled(node: &StructureNode, label: &str) -> bool {
    if node.kind == NodeKind::Section {
        node.fragment().is_some_and(|place| equal_fold(&place, label))
    } else {
        node.label.as_deref().is_some_and(|own| equal_fold(own, label))
    }
}

/// Whether `label` names a node: a prose block by the label its publisher anchors it by, exactly; any other by its label
/// (`labelled`) or an alias, a provision's also under its kind's mark or as it is ("section 5(2)").
fn names(node: &StructureNode, label: &str) -> bool {
    let own = if node.kind == NodeKind::Prose && node.label.is_some() && node.label == node.anchor {
        node.label.as_deref() == Some(label)
    } else {
        labelled(node, label)
    };
    let mark = if node.kind == NodeKind::Section { provision_mark(node.locator_kind.as_deref()) } else { "" };
    own || node.aliases.as_deref().unwrap_or_default().iter()
        .any(|alias| equal_fold(alias, label) || !mark.is_empty() && equal_fold(&format!("{mark}{alias}"), label))
}

/// The indexes of the nodes the query serves, in its order.
fn projected_positions(document: &DocumentStructure) -> Vec<usize> {
    let mut positions = document
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| projected(node) && (node.kind == NodeKind::Prose || node.label.is_some()))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let key = |index: &usize| {
        let node = &document.nodes[*index];
        (document.query_range(node), node.label.as_deref().unwrap_or_default())
    };
    match projection_order(document.profile) {
        ProjectionOrder::StablePosition => positions.sort_by_key(|index| {
            let (range, _) = key(index);
            (range.start, range.end)
        }),
        ProjectionOrder::Position => positions.sort_by_key(|index| {
            let (range, label) = key(index);
            (range.start, range.end, label)
        }),
        ProjectionOrder::Legislation => positions.sort_by_key(|index| {
            let (range, label) = key(index);
            (range.start, std::cmp::Reverse(range.end), label)
        }),
        ProjectionOrder::Case | ProjectionOrder::Native => {}
    }
    positions
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentViewerSlice<'a> {
    pub start: usize,
    pub end: usize,
    pub text: &'a str,
    pub anchors: Vec<legal_structure_model::NodeReading<'a>>,
    pub primary: Option<legal_structure_model::NodeReading<'a>>,
    /// The slice is a heading the structure reads, not a block of text.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<bool>", optional))]
    pub heading: bool,
    /// The marker its primary node prints ("(2)", "[12]"), emphasis marks aside, and where in `text` (UTF-16) the words
    /// after it begin: 0 where the text does not open on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub marker: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<usize>", optional))]
    pub body: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
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
    blocks: OnceLock<Vec<usize>>,
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

    fn positions<'a>(&'a self, document: &DocumentStructure) -> &'a [usize] {
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
        let mut window = self.text_window_with_text(document, &scalar, offset, start_char, limit);
        self.describe_rows(document, &mut window);
        window
    }

    /// Each row's block, locator and table cells, read where its span stands.
    fn describe_rows<'a>(&self, document: &'a DocumentStructure, window: &mut DocumentTextWindow<'a>) {
        if window.rows.is_empty() {
            return;
        }
        let cells = self.table_cells(document);
        let text = self.text(document);
        for row in &mut window.rows {
            let [start, end] = row.span;
            let block = self.smallest_containing(document, start, end);
            row.locator = self.locator_of(document, start, end, block.map(|index| &document.nodes[index]));
            row.block = block.map(|index| self.materialize(document, index, &text));
            row.cells = cells.iter().flat_map(|cell| cell.ranges.iter().filter_map(move |range| {
                let (from, to) = (range.start.max(start), range.end.min(end));
                (from < to).then(|| RowCell { start: from, end: to,
                    table_name: cell.table_name.clone().unwrap_or_default(),
                    address: cell.address.clone().unwrap_or_default() })
            })).collect();
        }
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
                    block: None,
                    locator: None,
                    cells: Vec::new(),
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
        self.describe_rows(document, &mut window);
        window
    }

    pub fn viewer<'a>(
        &'a self,
        document: &'a DocumentStructure,
        primary_kind: NodeKind,
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
            .blocks(document, None)
            .filter(|node| {
                document.query_range(node).start < end && (node.kind == NodeKind::Page || queried_as(node, primary_kind))
            })
            .collect::<Vec<_>>();
        let mut grouped = BTreeMap::<usize, Vec<&StructureNode>>::new();
        let mut breaks = Vec::new();
        for node in anchors {
            let range = document.query_range(node);
            if queried_as(node, primary_kind) {
                breaks.push(range.end.min(end));
            }
            grouped.entry(range.start).or_default().push(node);
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
                if queried_as(anchor, primary_kind) {
                    // (A provision's by its place, its kind's mark and label: a part's over the section opening it.)
                    let named = if anchor.kind == NodeKind::Section { anchor.fragment() } else { anchor.label.clone() };
                    let length = crate::text::utf16_len(named.as_deref().unwrap_or_default());
                    if primary.is_none() || length > primary_length {
                        primary = Some(index);
                        primary_length = length;
                    }
                }
            }
            if primary.is_none() {
                primary = anchors
                    .iter()
                    .position(|anchor| anchor.kind == NodeKind::Page);
            }
            let primary = primary.map(|index| anchors.remove(index));
            let slice_end = starts[index + 1];
            let raw = scalar.slice_utf16(start..slice_end).unwrap_or_default();
            let slice_text = js_trim(raw);
            // The primary's printed marker, where it stands in the slice; the source's text is the query's.
            let leading = crate::utf16_len(&raw[..raw.len() - raw.trim_start_matches(crate::javascript_whitespace).len()]);
            let marker = primary.filter(|_| document.rendered_text.is_none()).and_then(|node| node.marker_range)
                .filter(|marker| start <= marker.start && marker.start < marker.end && marker.end <= slice_end);
            let body = marker.filter(|marker| marker.start == start + leading).map_or(0, |marker| {
                let after = scalar.slice_utf16(marker.end..slice_end).unwrap_or_default();
                marker.end - start - leading
                    + crate::utf16_len(&after[..after.len() - after.trim_start_matches(crate::javascript_whitespace).len()])
            });
            let marker = marker.and_then(|marker| scalar.slice_utf16(marker.start..marker.end))
                .map(|printed| js_trim(printed).trim_matches(['*', '_']).to_owned()).filter(|printed| !printed.is_empty());
            if !slice_text.is_empty() || primary.is_some() || !anchors.is_empty() {
                slices.push(DocumentViewerSlice {
                    start,
                    end: slice_end,
                    text: slice_text,
                    anchors: anchors.into_iter().map(|anchor| document.reading(anchor)).collect(),
                    heading: primary.is_none() && headings.contains(&start),
                    primary: primary.map(|anchor| document.reading(anchor)),
                    marker,
                    body: body.min(crate::utf16_len(slice_text)),
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

    fn materialize<'a>(&self, document: &'a DocumentStructure, index: usize, text: &ScalarText<'_>) -> Passage<'a> {
        let reading = document.reading(&document.nodes[index]);
        Passage { text: crate::text::membership_text_utf16(text, &reading.ranges), reading }
    }

    /// The nodes the query serves, in its order (`kind`: those served as it).
    pub fn blocks<'a>(
        &'a self,
        document: &'a DocumentStructure,
        kind: Option<NodeKind>,
    ) -> impl Iterator<Item = &'a StructureNode> + 'a {
        self.positions(document)
            .iter()
            .map(|&index| &document.nodes[index])
            .filter(move |node| kind.is_none_or(|kind| queried_as(node, kind)))
    }

    /// The indexes of the nodes the query serves as `kind`, in its order.
    fn served(&self, document: &DocumentStructure, kind: NodeKind) -> Vec<usize> {
        self.positions(document).iter().copied().filter(|&index| queried_as(&document.nodes[index], kind)).collect()
    }

    fn resolves(&self, document: &DocumentStructure, node: &StructureNode, label: &str, kind: Option<NodeKind>) -> bool {
        names(node, label)
            || kind == Some(NodeKind::Footnote) && note_named(node, &legal_locator::note_label(label))
            || (node.anchor.as_deref() == Some(label)
                // Publisher IDs and legal labels are separate namespaces: a
                // displayed footnote 1 can have browser ID fn0, while fn1 is
                // another footnote's ID. A known legal label in the requested
                // kind takes precedence; untyped graph lookup considers all kinds.
                && !self.blocks(document, kind).any(|candidate| names(candidate, label)))
    }

    fn unique_position(&self, document: &DocumentStructure, label: &str, kind: Option<NodeKind>) -> Option<usize> {
        let mut matches = self.positions(document).iter().copied().filter(|&index| {
            let node = &document.nodes[index];
            kind.is_none_or(|kind| queried_as(node, kind)) && self.resolves(document, node, label, kind)
        });
        let found = matches.next()?;
        matches.next().is_none().then_some(found)
    }

    fn subtree_positions(&self, document: &DocumentStructure, seed: usize) -> Vec<usize> {
        let by_id = document.nodes.iter().enumerate()
            .map(|(index, node)| (node.id.as_str(), index)).collect::<HashMap<_, _>>();
        self.positions(document).iter().copied().filter(|&position| {
            let mut current = Some(position);
            let mut seen = HashSet::new();
            while let Some(index) = current {
                if !seen.insert(index) { break; }
                if index == seed { return true; }
                current = document.nodes[index].parent_id.as_deref()
                    .and_then(|parent| by_id.get(parent).copied());
            }
            false
        }).collect()
    }

    pub fn has_origin(&self, document: &DocumentStructure, origin: Derivation) -> bool {
        self.blocks(document, None).any(|node| node.source == origin)
    }

    fn requested_label<'a>(
        &self,
        document: &DocumentStructure,
        kind: NodeKind,
        locator: &'a str,
    ) -> Cow<'a, str> {
        let exact = js_trim(locator);
        if self.blocks(document, Some(kind)).any(|node| labelled(node, exact)) {
            return Cow::Borrowed(exact);
        }
        let normalized = normalize_document_locator(kind, locator);
        if !normalized.is_empty() {
            return Cow::Owned(normalized);
        }
        self.blocks(document, Some(kind))
            .any(|node| {
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
            .or_else(|| (kind == NodeKind::Footnote).then(|| Cow::Owned(legal_locator::note_label(locator))))
            .unwrap_or_default()
    }

    /// The lookup of `selected`, one of `available`, with `context` of its neighbours each side.
    fn found<'a>(
        &self,
        document: &'a DocumentStructure,
        requested_label: String,
        available: &[usize],
        selected: usize,
        context_blocks: usize,
    ) -> DocumentLookup<'a> {
        let order = available.iter().position(|&candidate| candidate == selected).unwrap_or(0);
        let context = context(context_blocks);
        let text = self.text(document);
        let passages = |indexes: &[usize]| indexes.iter().map(|&index| self.materialize(document, index, &text)).collect();
        DocumentLookup {
            status: DocumentLookupStatus::Found,
            requested_label,
            matches: vec![document.nodes[selected].label.clone().unwrap_or_default()],
            block: Some(self.materialize(document, selected, &text)),
            before: passages(&available[order.saturating_sub(context)..order]),
            after: passages(&available[order + 1..(order + 1 + context).min(available.len())]),
        }
    }

    fn lookup_label<'a>(
        &self,
        document: &'a DocumentStructure,
        kind: NodeKind,
        requested_label: &str,
        context_blocks: usize,
    ) -> DocumentLookup<'a> {
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
        let available = self.served(document, kind);
        if available.is_empty() {
            return empty(DocumentLookupStatus::Unavailable);
        }
        let mut resolved = available.iter().copied()
            .filter(|&index| self.resolves(document, &document.nodes[index], requested_label, Some(kind)));
        let (Some(selected), None) = (resolved.next(), resolved.next()) else {
            let matches = available
                .iter()
                .map(|&index| &document.nodes[index])
                .filter(|node| names(node, requested_label))
                .map(|node| node.label.clone().unwrap_or_default())
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
        self.found(document, requested_label.to_owned(), &available, selected, context_blocks)
    }

    pub fn read_range<'a>(
        &self,
        document: &'a DocumentStructure,
        kind: NodeKind,
        from: &str,
        to: &str,
        context_blocks: usize,
    ) -> Option<DocumentRangeLookup<'a>> {
        let available = self.served(document, kind);
        let resolve = |locator: &str| {
            let label = self.requested_label(document, kind, locator);
            let index = self.unique_position(document, &label, Some(kind))?;
            available.iter().position(|&candidate| candidate == index)
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

    fn contained_leaf_blocks(&self, document: &DocumentStructure, blocks: &[usize]) -> Vec<usize> {
        // (Prose is read among paragraphs, as paragraphs are asked for.)
        let Some(kind) = blocks.first().map(|&index| match document.nodes[index].kind {
            NodeKind::Prose => NodeKind::Paragraph,
            kind => kind,
        }) else {
            return Vec::new();
        };
        // Parallel reporter pages may overlap without owning one another.
        if kind == NodeKind::Page {
            return blocks.to_vec();
        }
        let mut ranges = blocks
            .iter()
            .flat_map(|&index| document.query_ranges(&document.nodes[index]).into_owned())
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
            .served(document, kind)
            .into_iter()
            .filter(|&index| {
                let node = &document.nodes[index];
                let range = document.query_range(node);
                // A point at a paragraph boundary is not contained prose from
                // the adjacent paragraph. Keep it when explicitly selected.
                if range.start == range.end {
                    return blocks.contains(&index);
                }
                document.query_ranges(node).iter().all(|part| {
                    let count = ranges.partition_point(|(start, _)| *start <= part.start);
                    count > 0 && ranges[count - 1].1 >= part.end
                })
            })
            .collect::<Vec<_>>();
        if kind != NodeKind::Section || contained.len() <= 1 {
            return contained;
        }
        // Range membership determines eligibility; only source parent edges
        // determine ancestry, including through unprojected intermediate nodes.
        // An ancestor whose own words stop where its first subprovision opens
        // ("(3) The Minister may" over "(a) …") is a leaf of its own.
        let by_id = document.nodes.iter().enumerate()
            .map(|(index, node)| (node.id.as_str(), index)).collect::<HashMap<_, _>>();
        let eligible = contained.iter().copied().collect::<HashSet<_>>();
        let mut parents = HashSet::new();
        for &candidate in &contained {
            let held = document.query_range(&document.nodes[candidate]);
            let mut current = document.nodes[candidate].parent_id.as_deref()
                .and_then(|parent| by_id.get(parent).copied());
            let mut seen = HashSet::from([candidate]);
            while let Some(index) = current {
                if !seen.insert(index) { break; }
                if eligible.contains(&index) && document.query_ranges(&document.nodes[index]).iter()
                    .any(|range| range.start <= held.start && held.end <= range.end) {
                    parents.insert(index);
                }
                current = document.nodes[index].parent_id.as_deref()
                    .and_then(|parent| by_id.get(parent).copied());
            }
        }
        contained
            .into_iter()
            .filter(|candidate| !parents.contains(candidate))
            .collect()
    }

    fn materialize_leaf_blocks<'a>(
        &self,
        document: &'a DocumentStructure,
        blocks: &[usize],
        text: &ScalarText<'_>,
    ) -> Vec<Passage<'a>> {
        self.contained_leaf_blocks(document, blocks)
            .into_iter()
            .map(|index| self.materialize(document, index, text))
            .filter(|unit| !unit.text.is_empty() || unit.reading.node.anchor.is_some())
            .collect()
    }

    /// The smallest node the query serves that holds `start..end` in one of its ranges.
    fn smallest_containing(&self, document: &DocumentStructure, start: usize, end: usize) -> Option<usize> {
        self.positions(document)
            .iter()
            .filter_map(|&index| {
                let ranges = document.query_ranges(&document.nodes[index]);
                ranges.iter().any(|range| range.start <= start && range.end >= end)
                    .then_some((index, ranges.iter().map(|range| range.end - range.start).sum::<usize>()))
            })
            .min_by_key(|(_, length)| *length)
            .map(|(index, _)| index)
    }

    pub fn smallest_containing_block<'a>(
        &self,
        document: &'a DocumentStructure,
        start: usize,
        end: usize,
    ) -> Option<Passage<'a>> {
        let index = self.smallest_containing(document, start, end)?;
        Some(self.materialize(document, index, &self.text(document)))
    }

    /// The page that holds `start..end`, else the first that shares any of it.
    fn page_at<'a>(&'a self, document: &'a DocumentStructure, start: usize, end: usize) -> Option<&'a StructureNode> {
        let page = |holds: &dyn Fn(&ScalarRange) -> bool| self.blocks(document, Some(NodeKind::Page))
            .find(|page| document.query_ranges(page).iter().any(holds));
        page(&|range| range.start <= start && range.end >= end)
            .or_else(|| page(&|range| range.start < end && range.end > start))
    }

    /// The legal locator `start..end` is cited by: the node `id` names (else the smallest block holding the span) where
    /// it is a labelled page, or a paragraph, provision or note that prints its label where it stands; else the
    /// labelled page holding the span (`page_at`); none where no page is labelled.
    pub fn locator(&self, document: &DocumentStructure, start: usize, end: usize, id: Option<&str>) -> Option<Locator> {
        let node = match id {
            Some(id) => document.nodes.iter().find(|node| node.id == id),
            None => self.smallest_containing(document, start, end).map(|index| &document.nodes[index]),
        };
        self.locator_of(document, start, end, node)
    }

    fn locator_of(&self, document: &DocumentStructure, start: usize, end: usize, node: Option<&StructureNode>)
        -> Option<Locator> {
        let own = node.filter(|node| node.label.is_some() && (node.kind == NodeKind::Page || node.prints_label()
            && matches!(node.kind, NodeKind::Paragraph | NodeKind::Section | NodeKind::Footnote)));
        let node = own.or_else(|| self.page_at(document, start, end).filter(|page| page.label.is_some()))?;
        Some(Locator { kind: node.kind, label: node.label.clone().unwrap_or_default(), fragment: node.fragment() })
    }

    /// The page `requested` names ("pdf:3", "printed:iv", "12"): a PDF page by its number, a printed page by its
    /// printed label (an alias); a bare number names the PDF page, or the printed page of a page no PDF numbers.
    fn page_address(&self, document: &DocumentStructure, requested: &str) -> DocumentAddressResolution {
        static QUALIFIED: OnceLock<Regex> = OnceLock::new();
        let raw = js_trim(requested);
        let qualified = regex_parts(
            &[r"(?iu)^(pdf|printed)", JS_WS, r"*[:=]", JS_WS, r"*(.+)$"],
            &QUALIFIED,
        )
        .captures(raw);
        let wanted = js_trim(qualified.as_ref().map_or(raw, |capture| &capture[2]));
        let pdf = qualified.as_ref().map_or_else(
            || !wanted.is_empty() && wanted.len() <= 6 && wanted.bytes().all(|byte| byte.is_ascii_digit()),
            |capture| capture[1].eq_ignore_ascii_case("pdf"),
        );
        let wanted_pdf = if !pdf {
            None
        } else if wanted == "null" {
            Some(None)
        } else {
            wanted.parse::<usize>().ok().filter(|value| value.to_string() == wanted).map(Some)
        };
        let printed = |page: &StructureNode| {
            !wanted.is_empty() && page.aliases.iter().flatten().any(|alias| equal_fold(js_trim(alias), wanted))
        };
        let mut pages = self.blocks(document, Some(NodeKind::Page)).peekable();
        if pages.peek().is_none() {
            return DocumentAddressResolution::NoPages;
        }
        let mut matches = pages.filter(|page| match page.page_indexes.first().map(|index| index + 1) {
            None if pdf && qualified.is_none() => printed(page),
            pdf_page if pdf => wanted_pdf == Some(pdf_page),
            _ => printed(page),
        });
        match (matches.next(), matches.next()) {
            (Some(page), None) => DocumentAddressResolution::Found { spans: document.query_ranges(page).into_owned() },
            (Some(_), Some(_)) => DocumentAddressResolution::Ambiguous,
            (None, _) => DocumentAddressResolution::NotFound,
        }
    }

    /// Resolve an existing node identity without crossing display-label namespaces.
    pub fn structure_selection<'a>(
        &self,
        document: &'a DocumentStructure,
        selector: &GraphScopeSeed,
        context_blocks: usize,
    ) -> DocumentLookup<'a> {
        let id = match selector {
            GraphScopeSeed::NodeId(id) => id,
            GraphScopeSeed::Locator(locator) => return self.structure_block(document, locator, context_blocks),
        };
        let Some(selected) = self.positions(document).iter().copied().find(|&index| document.nodes[index].id == *id) else {
            return DocumentLookup { status: DocumentLookupStatus::NotFound,
                requested_label: id.clone(), matches: Vec::new(), block: None,
                before: Vec::new(), after: Vec::new() };
        };
        let available = self.served(document, document.nodes[selected].kind);
        self.found(document, id.clone(), &available, selected, context_blocks)
    }

    pub fn structure_block<'a>(
        &self,
        document: &'a DocumentStructure,
        locator: &str,
        context_blocks: usize,
    ) -> DocumentLookup<'a> {
        let direct = js_trim(locator).to_lowercase();
        if direct.starts_with("table:") {
            let kind = if direct.contains("/col:") {
                NodeKind::Cell
            } else if direct.contains("/row:") {
                NodeKind::Row
            } else {
                NodeKind::Table
            };
            return self.lookup_label(document, kind, &direct, context_blocks);
        }
        let normalized = normalize_document_locator(NodeKind::Section, locator);
        let requested_label = if normalized.is_empty() { direct } else { normalized };
        let empty = |status, matches| DocumentLookup {
            status, requested_label: requested_label.clone(), matches, block: None, before: Vec::new(), after: Vec::new(),
        };
        if requested_label.is_empty() {
            return empty(DocumentLookupStatus::Unavailable, Vec::new());
        }
        match self.provisions(document, "section", locator, None, None, false) {
            Provisions::Found { order, first, .. } => {
                let selected = order[first];
                let served = self.served(document, NodeKind::Section);
                let available = if served.contains(&selected) { served } else { vec![selected] };
                self.found(document, requested_label.clone(), &available, selected, context_blocks)
            }
            Provisions::Ambiguous(nodes) => empty(DocumentLookupStatus::Ambiguous,
                nodes.into_iter().map(|index| document.nodes[index].label.clone().unwrap_or_default()).collect()),
            Provisions::NotFound => empty(DocumentLookupStatus::NotFound, Vec::new()),
            Provisions::Unavailable => empty(DocumentLookupStatus::Unavailable, Vec::new()),
        }
    }

    pub fn graph_scope<'a>(
        &self,
        document: &'a DocumentStructure,
        graph: &InstrumentCrossReferenceGraph,
        seed: &GraphScopeSeed,
        follow: FollowDirection,
        depth: usize,
        include_descendants: bool,
        include_units: bool,
    ) -> Option<GraphScope<'a>> {
        let seed = match seed {
            GraphScopeSeed::Locator(locator) => self.unique_position(document, js_trim(locator), None)?,
            GraphScopeSeed::NodeId(id) => self.positions(document).iter().copied()
                .find(|&index| document.nodes[index].id == *id)?,
        };
        self.graph_scope_position(document, graph, seed, follow, depth, include_descendants, include_units)
    }

    fn graph_scope_position<'a>(
        &self,
        document: &'a DocumentStructure,
        graph: &InstrumentCrossReferenceGraph,
        seed: usize,
        follow: FollowDirection,
        depth: usize,
        include_descendants: bool,
        include_units: bool,
    ) -> Option<GraphScope<'a>> {
        let limit = depth.min(3);
        let initial = if include_descendants {
            self.subtree_positions(document, seed)
        } else {
            vec![seed]
        };
        // Labels are display locators, not node identities. Edge coordinates must
        // identify exactly one reflected owner, including across repeated labels.
        let endpoint = |label: &str, start: usize, end: usize, target: bool| {
            let mut matches = self.positions(document).iter().copied().filter(|&index| {
                let node = &document.nodes[index];
                if node.kind != NodeKind::Section
                    || !equal_fold(node.label.as_deref().unwrap_or_default(), label) { return false; }
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
        let mut reached = vec![seed];
        let mut reached_nodes = initial.iter().copied().collect::<HashSet<_>>();
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
                    && source.is_some_and(|source| frontier.contains(&source));
                let backward = matches!(follow, FollowDirection::In | FollowDirection::Both)
                    && target.is_some_and(|target| frontier.contains(&target));
                if !forward && !backward { continue; }
                let (Some(source), Some(target), true) =
                    (source, target, edge.status == InstrumentCrossReferenceStatus::Resolved) else {
                    if omitted_edges.insert(edge_index) {
                        omissions.push(GraphScopeOmission {
                            edge_index,
                            edge: edge.clone(),
                            source_node_id: source.map(|source| document.nodes[source].id.clone()),
                            target_node_id: target.map(|target| document.nodes[target].id.clone()),
                            missing_endpoints: [("source", source.is_none()), ("target", target.is_none())]
                                .into_iter().filter_map(|(endpoint, missing)| missing.then_some(endpoint)).collect(),
                        });
                    }
                    continue;
                };
                let other = if forward { target } else { source };
                if reached_nodes.insert(other) {
                    reached.push(other);
                    next.insert(other);
                }
            }
            frontier = next;
            hops += 1;
        }
        let mut rest = reached
            .into_iter()
            .filter(|&index| index != seed)
            .collect::<Vec<_>>();
        rest.sort_by_key(|&index| document.query_range(&document.nodes[index]).start);
        let text = self.text(document);
        let seed = self.materialize(document, seed, &text);
        Some(GraphScope {
            seed,
            nodes: rest
                .iter()
                .map(|&index| {
                    let materialized = self.materialize(document, index, &text);
                    let units = include_units
                        .then(|| self.materialize_leaf_blocks(document, &[index], &text));
                    let units = units.filter(|units| {
                        units.len() != 1 || units[0].reading.span != materialized.reading.span
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
            Some(DocumentAddress::Page { spec }) => self.page_address(document, &spec),
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
                let mut spans = block.reading.ranges.clone().into_owned();
                if follow != FollowDirection::None {
                    let Some(graph) = document
                        .cross_references
                        .as_ref()
                        .filter(|graph| !graph.document_abstained)
                    else {
                        return DocumentAddressResolution::NotAddressable;
                    };
                    let Some(seed) = self.positions(document).iter().copied().find(|&index|
                        std::ptr::eq(block.reading.node, &document.nodes[index]))
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
                    spans.extend(scope.nodes.into_iter().flat_map(|node| node.block.reading.ranges.into_owned()));
                }
                DocumentAddressResolution::Found { spans }
            }
        }
    }
}

/// The label a typed locator names among a document's nodes of `kind`: a paragraph's, page's or note's number as it prints
/// ("para. 12" is "12"), a provision's place ("s. 5(2)" is "sec5(2)"); empty where it names none.
pub fn normalize_document_locator(kind: NodeKind, locator: &str) -> String {
    match kind {
        NodeKind::Paragraph | NodeKind::Page | NodeKind::Footnote => {
            parse_ordinal(kind.name(), locator).map_or_else(String::new, |number| number.to_string())
        }
        NodeKind::Section => normalize_section_locator(locator),
        _ => String::new(),
    }
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
