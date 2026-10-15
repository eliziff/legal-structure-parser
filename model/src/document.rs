use crate::{
    DefinedTerm, DetectionProfile, DocxStructureFacts, EvidenceKind, InstrumentContentsReading,
    InstrumentCrossReferenceGraph, Origin, ResolutionProofV2, ScalarRange, ScalarText, Scope,
    DOCUMENT_STRUCTURE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Paragraph,
    Page,
    Section,
    Heading,
    Footnote,
    Endnote,
    Prose,
    List,
    ListItem,
    Table,
    Row,
    Cell,
}

impl NodeKind {
    /// Grid containment identifies physical objects, not a semantic outline attachment.
    /// Cell contents may carry independent document, quotation or reading-stream ownership.
    pub fn is_grid_child_of(self, parent: Self) -> bool {
        matches!((self, parent), (Self::Cell, Self::Row) | (Self::Row, Self::Table))
    }

    pub fn evidence(self) -> EvidenceKind {
        match self {
            Self::Paragraph => EvidenceKind::Paragraph,
            Self::Prose => EvidenceKind::Prose,
            Self::Page => EvidenceKind::Page,
            Self::Section => EvidenceKind::Section,
            Self::Heading => EvidenceKind::Heading,
            Self::Footnote => EvidenceKind::Footnote,
            Self::Endnote => EvidenceKind::Endnote,
            Self::List | Self::ListItem => EvidenceKind::List,
            Self::Table => EvidenceKind::Table,
            Self::Row => EvidenceKind::Row,
            Self::Cell => EvidenceKind::Cell,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ListItem => "list_item",
            _ => self.evidence().name(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Derivation {
    Native,
    Heuristic,
    Model,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKindV2 {
    Footnote,
    Endnote,
    Sidenote,
    TableNote,
    AuthorNote,
}

/// Functional roles are independent of locators and structural object kinds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryRole {
    Text,
    Heading,
    Caption,
    Instruction,
    Headnote,
    MarginalNote,
    Furniture,
    PageLabel,
}

/// Outline relationships may not cross any of these independently owned scopes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructureScopeKind { Document, Quotation, ReadingStream }

/// Supplied or inferred scope identity and exact membership; absent metadata stays absent.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StructureScopeAnnotation {
    pub id: String,
    pub kind: StructureScopeKind,
    pub ownership: StructureOwnership,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_scope_id: Option<String>,
    pub ranges: Vec<ScalarRange>,
    /// Exact members owned by this document, excluding nested constituent documents.
    #[serde(default)]
    pub owned_ranges: Vec<ScalarRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub title_ranges: Vec<ScalarRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attribution_ranges: Vec<ScalarRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StructureOwnership {
    pub document_id: String,
    pub quotation_id: Option<String>,
    pub reading_stream_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructureNode {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<PrimaryRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<StructureOwnership>,
    /// Exact physical membership; range remains its envelope for positioning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<ScalarRange>,
    /// Heading text, distinct from the extent of its descendants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_range: Option<ScalarRange>,
    /// Derived from parents, never an independently authored depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<usize>,
    /// Derived exact membership of this node and its descendants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extent: Vec<ScalarRange>,
    pub kind: NodeKind,
    pub range: ScalarRange,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rendered_range: Option<ScalarRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rendered_ranges: Vec<ScalarRange>,
    /// Own logical content, before adding descendant section membership.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rendered_membership: Vec<ScalarRange>,
    pub origin_id: Cow<'static, str>,
    pub source: Derivation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub markup_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub markup_start: Option<usize>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub markup_attributes: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker_range: Option<ScalarRange>,
    /// One-based public table number, assigned by the table producer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_number: Option<usize>,
    /// One-based header row coordinates; None means the producer has not classified them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_rows: Option<Vec<usize>>,
    /// One-based grid position; merged cells name their top-left position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_value: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub page_indexes: Vec<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub line_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grammar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<ResolutionProofV2>,
}

impl StructureNode {
    pub fn new(
        id: String,
        kind: NodeKind,
        range: ScalarRange,
        origin_id: impl Into<Cow<'static, str>>,
        source: Derivation,
        parent_id: Option<String>,
    ) -> Self {
        Self {
            id,
            role: None,
            ownership: None,
            ranges: Vec::new(),
            heading_range: None,
            level: None,
            extent: Vec::new(),
            kind,
            range,
            rendered_range: None,
            rendered_ranges: Vec::new(),
            rendered_membership: Vec::new(),
            origin_id: origin_id.into(),
            source,
            label: None,
            locator_kind: None,
            aliases: None,
            parent_id,
            anchor: None,
            markup_tag: None,
            markup_start: None,
            markup_attributes: BTreeMap::new(),
            content_start: None,
            marker_range: None,
            table_number: None,
            header_rows: None,
            row: None,
            column: None,
            row_span: None,
            column_span: None,
            display_value: None,
            page_indexes: Vec::new(),
            line_ids: Vec::new(),
            grammar: None,
            proof: None,
        }
    }
}

pub fn node_depths<'a>(nodes: &'a [StructureNode]) -> HashMap<&'a str, usize> {
    let parents = nodes
        .iter()
        .map(|node| (node.id.as_str(), node.parent_id.as_deref()))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut depths = HashMap::with_capacity(nodes.len());
    for node in nodes {
        seen.clear();
        let mut depth = 0;
        let mut parent = node.parent_id.as_deref();
        while let Some(id) = parent.filter(|id| seen.insert(*id)) {
            depth += 1;
            parent = parents.get(id).copied().flatten();
        }
        depths.insert(node.id.as_str(), depth);
    }
    depths
}

pub fn public_structure_label(value: &str) -> Cow<'_, str> {
    if !value.contains('@') {
        return Cow::Borrowed(value);
    }
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('@') {
        result.push_str(&rest[..at]);
        let digits = rest[at + 1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits == 0 {
            result.push('@');
            rest = &rest[at + 1..];
        } else {
            rest = &rest[at + 1 + digits..];
        }
    }
    result.push_str(rest);
    Cow::Owned(result)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NoteReference {
    pub range: ScalarRange,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub page_indexes: Vec<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub line_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Note {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<StructureOwnership>,
    pub kind: NoteKindV2,
    pub label_range: ScalarRange,
    pub body_range: ScalarRange,
    /// Exact source membership; the singular ranges are positioning envelopes.
    #[serde(default)]
    pub label_ranges: Vec<ScalarRange>,
    #[serde(default)]
    pub body_ranges: Vec<ScalarRange>,
    pub references: Vec<NoteReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_reference: Option<ScalarRange>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CitedAuthority {
    pub citation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructureDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub ranges: Vec<ScalarRange>,
    pub node_ids: Vec<String>,
}

/// Source markup without an inferred legal role. Byte offsets address the
/// exact markup identified by `DocumentStructure::markup_sha256`; ranges
/// address the rendered UTF-16 text. Parent indexes refer to markup_elements.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MarkupElement {
    pub markup_start: usize,
    pub markup_end: usize,
    pub tag: String,
    pub attributes: BTreeMap<String, String>,
    pub parent: Option<usize>,
    pub range: ScalarRange,
}

/// Actual enclosing source elements, nearest parent first. They supply context
/// outside the markup input and have no invented body-text ranges.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MarkupAncestor {
    pub tag: String,
    pub attributes: BTreeMap<String, String>,
}

/// Actual PDF marked-content role; empty membership means the native tag remains unaligned.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PdfStructureTagWitness {
    pub page_index: usize,
    pub mcid: i64,
    pub role: String,
    pub ranges: Vec<ScalarRange>,
    pub line_ids: Vec<String>,
}

/// Physical text fragment in this identified source representation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFragment {
    pub id: String,
    pub page_index: usize,
    pub range: ScalarRange,
}


/// An explicit logical object link between physical source starts (UTF-16).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationKind { Paragraph, Heading, Note, Table, Quotation, List }

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceContinuation {
    pub kind: ContinuationKind,
    pub from: usize,
    pub to: usize,
    pub separator: Option<String>,
}

/// Line-local Unicode scalar edits for the logical view; printed source stays unchanged.
pub type ManuscriptEdits = BTreeMap<String, Vec<(usize, usize, String)>>;

/// A manuscript join is distinct from logical object identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceJoin {
    pub from: usize,
    pub to: usize,
    pub separator: String,
}

/// A source-backed form field, independent of the enclosing text/table structure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind { Text, Signature, Checkbox, Choice }

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldState { Checked, Unchecked }

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FieldAnnotation {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    pub ranges: Vec<ScalarRange>,
    pub label_ranges: Vec<ScalarRange>,
    pub value_ranges: Vec<ScalarRange>,
    pub placeholder_ranges: Vec<ScalarRange>,
    pub state: Option<FieldState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<StructureOwnership>,
}

/// Original PDF navigation metadata; its destination does not assert a visible heading.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeNavigationWitness {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub page_index: usize,
}

/// Recorded navigation targets. An empty target remains unresolved.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NavigationEntry {
    pub id: String,
    pub kind: String,
    pub source_ranges: Vec<ScalarRange>,
    #[serde(default)]
    pub title_ranges: Vec<ScalarRange>,
    pub source_line_ids: Vec<String>,
    pub target_ranges: Vec<ScalarRange>,
    pub target_line_ids: Vec<String>,
    pub parent_id: Option<String>,
    pub witness_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DocumentStructure {
    pub schema_version: Cow<'static, str>,
    pub document_id: String,
    pub offset_unit: Cow<'static, str>,
    #[serde(default)]
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub profile: Option<DetectionProfile>,
    #[serde(default)]
    pub revision: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rendered_text: Option<String>,
    pub text_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    pub scope: Scope,
    pub origins: Vec<Origin>,
    pub nodes: Vec<StructureNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_fragments: Vec<SourceFragment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_annotations: Vec<StructureScopeAnnotation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structure_tags: Vec<PdfStructureTagWitness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_navigation: Vec<NativeNavigationWitness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub navigation: Vec<NavigationEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldAnnotation>,
    /// None means no explicit source links were supplied, not an empty authored decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuations: Option<Vec<SourceContinuation>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joins: Vec<SourceJoin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub manuscript_edits: ManuscriptEdits,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub markup_elements: Vec<MarkupElement>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub markup_ancestors: Vec<MarkupAncestor>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub markup_sha256: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notes: Vec<Note>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub cited_authorities: Vec<CitedAuthority>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub definitions: Vec<DefinedTerm>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docx: Option<DocxStructureFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_hypothesis: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<InstrumentContentsReading>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_references: Option<InstrumentCrossReferenceGraph>,
    pub diagnostics: Vec<StructureDiagnostic>,
}

impl DocumentStructure {
    pub fn project_scalar_parts(
        coordinates: &ScalarText<'_>,
        nodes: &mut [StructureNode],
        notes: &mut [Note],
        diagnostics: &mut [StructureDiagnostic],
    ) {
        if coordinates.len() == coordinates.utf16_len() {
            return;
        }
        let utf16_range = |range: &mut ScalarRange| {
            range.start = coordinates.utf16(range.start);
            range.end = coordinates.utf16(range.end);
        };
        for node in nodes {
            utf16_range(&mut node.range);
            for range in &mut node.extent {
                utf16_range(range);
            }
            for range in &mut node.ranges {
                utf16_range(range);
            }
            if let Some(range) = &mut node.heading_range {
                utf16_range(range);
            }
            if let Some(start) = &mut node.content_start {
                *start = coordinates.utf16(*start);
            }
            if let Some(range) = &mut node.marker_range {
                utf16_range(range);
            }
        }
        for note in notes {
            utf16_range(&mut note.label_range);
            utf16_range(&mut note.body_range);
            for range in note.label_ranges.iter_mut().chain(&mut note.body_ranges) {
                utf16_range(range);
            }
            for reference in &mut note.references {
                utf16_range(&mut reference.range);
            }
            if let Some(reference) = &mut note.primary_reference {
                utf16_range(reference);
            }
        }
        for diagnostic in diagnostics {
            for range in &mut diagnostic.ranges {
                utf16_range(range);
            }
        }
    }

    pub fn from_scalar_parts(
        document_id: String,
        provider: String,
        text: String,
        text_sha256: String,
        source_sha256: Option<String>,
        scope: Scope,
        origins: Vec<Origin>,
        mut nodes: Vec<StructureNode>,
        mut notes: Vec<Note>,
        mut diagnostics: Vec<StructureDiagnostic>,
    ) -> Self {
        let coordinates = ScalarText::new(&text);
        Self::project_scalar_parts(&coordinates, &mut nodes, &mut notes, &mut diagnostics);
        Self::from_projected_parts(
            document_id,
            provider,
            None,
            None,
            None,
            text,
            text_sha256,
            source_sha256,
            scope,
            origins,
            nodes,
            notes,
            diagnostics,
        )
    }

    pub fn from_projected_parts(
        document_id: String,
        provider: String,
        url: Option<String>,
        doc_type: Option<String>,
        profile: Option<DetectionProfile>,
        text: String,
        text_sha256: String,
        source_sha256: Option<String>,
        scope: Scope,
        origins: Vec<Origin>,
        nodes: Vec<StructureNode>,
        notes: Vec<Note>,
        diagnostics: Vec<StructureDiagnostic>,
    ) -> Self {
        let mut structure = Self {
            schema_version: Cow::Borrowed(DOCUMENT_STRUCTURE_SCHEMA),
            document_id,
            offset_unit: Cow::Borrowed("utf16"),
            provider,
            url,
            doc_type,
            profile,
            revision: text_sha256.clone(),
            text,
            rendered_text: None,
            text_sha256,
            source_sha256,
            scope,
            origins,
            nodes,
            source_fragments: Vec::new(),
            scope_annotations: Vec::new(),
            structure_tags: Vec::new(),
            native_navigation: Vec::new(),
            navigation: Vec::new(),
            fields: Vec::new(),
            continuations: None,
            joins: Vec::new(),
            manuscript_edits: BTreeMap::new(),
            markup_elements: Vec::new(),
            markup_ancestors: Vec::new(),
            markup_sha256: None,
            notes,
            cited_authorities: Vec::new(),
            definitions: Vec::new(),
            docx: None,
            selected_hypothesis: None,
            contents: None,
            cross_references: None,
            diagnostics,
        };
        structure.refresh_revision().expect("Typed document structure is serializable");
        structure
    }

    /// Identity of the complete queryable representation, excluding the prior digest itself.
    pub fn refresh_revision(&mut self) -> Result<(), serde_json::Error> {
        use sha2::{Digest, Sha256};
        let mut representation = serde_json::to_value(&*self)?;
        representation.as_object_mut().unwrap().remove("revision");
        self.revision = format!("{:x}", Sha256::digest(serde_json::to_vec(&representation)?));
        Ok(())
    }

    pub fn query_text(&self) -> &str {
        self.rendered_text.as_deref().unwrap_or(&self.text)
    }

    /// Exact readable parts, kept separate from the envelope used to position a node.
    pub fn query_ranges<'a>(&self, node: &'a StructureNode) -> Cow<'a, [ScalarRange]> {
        if !node.rendered_ranges.is_empty() {
            return Cow::Borrowed(&node.rendered_ranges);
        }
        if let Some(range) = node.rendered_range {
            return Cow::Owned(vec![range]);
        }
        if (matches!(node.kind, NodeKind::Heading | NodeKind::Section)
            || node.role == Some(PrimaryRole::Heading)) && !node.extent.is_empty() {
            return Cow::Borrowed(&node.extent);
        }
        if !node.ranges.is_empty() {
            return Cow::Borrowed(&node.ranges);
        }
        Cow::Owned(vec![node.range])
    }

    pub fn query_range(&self, node: &StructureNode) -> ScalarRange {
        let ranges = self.query_ranges(node);
        ScalarRange {
            start: ranges
                .iter()
                .map(|range| range.start)
                .min()
                .unwrap_or(node.range.start),
            end: ranges
                .iter()
                .map(|range| range.end)
                .max()
                .unwrap_or(node.range.end),
        }
    }
}
