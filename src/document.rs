use crate::{
    DefinedTerm, DetectionProfile, DocxStructureFacts, EvidenceKind, InstrumentContentsReading,
    InstrumentCrossReferenceGraph, Origin, ResolutionProofV2, ScalarRange, ScalarText, Scope,
    DOCUMENT_STRUCTURE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
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
    pub(crate) fn evidence(self) -> EvidenceKind {
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

    pub(crate) fn name(self) -> &'static str {
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructureNode {
    pub id: String,
    pub kind: NodeKind,
    pub range: ScalarRange,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rendered_range: Option<ScalarRange>,
    pub origin_id: Cow<'static, str>,
    pub source: Derivation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker_range: Option<ScalarRange>,
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
            kind,
            range,
            rendered_range: None,
            origin_id: origin_id.into(),
            source,
            label: None,
            locator_kind: None,
            aliases: None,
            parent_id,
            anchor: None,
            content_start: None,
            marker_range: None,
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

pub(crate) fn node_depths<'a>(nodes: &'a [StructureNode]) -> HashMap<&'a str, usize> {
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

pub(crate) fn public_structure_label(value: &str) -> Cow<'_, str> {
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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub page_indexes: Vec<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub line_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Note {
    pub id: String,
    pub node_id: String,
    pub kind: NoteKindV2,
    pub label_range: ScalarRange,
    pub body_range: ScalarRange,
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
    pub(crate) fn project_scalar_parts(
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

    pub(crate) fn from_scalar_parts(
        document_id: String,
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

    pub(crate) fn from_projected_parts(
        document_id: String,
        text: String,
        text_sha256: String,
        source_sha256: Option<String>,
        scope: Scope,
        origins: Vec<Origin>,
        nodes: Vec<StructureNode>,
        notes: Vec<Note>,
        diagnostics: Vec<StructureDiagnostic>,
    ) -> Self {
        Self {
            schema_version: Cow::Borrowed(DOCUMENT_STRUCTURE_SCHEMA),
            document_id,
            offset_unit: Cow::Borrowed("utf16"),
            provider: "local-pdf".to_owned(),
            url: None,
            doc_type: None,
            profile: None,
            revision: text_sha256.clone(),
            text,
            rendered_text: None,
            text_sha256,
            source_sha256,
            scope,
            origins,
            nodes,
            notes,
            cited_authorities: Vec::new(),
            definitions: Vec::new(),
            docx: None,
            selected_hypothesis: None,
            contents: None,
            cross_references: None,
            diagnostics,
        }
    }

    pub fn query_text(&self) -> &str {
        self.rendered_text.as_deref().unwrap_or(&self.text)
    }

    pub(crate) fn query_range(&self, node: &StructureNode) -> ScalarRange {
        node.rendered_range.unwrap_or(node.range)
    }
}
