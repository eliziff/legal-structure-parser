use crate::{
    DefinedTerm, DetectionProfile, DocxStructureFacts, InstrumentContentsReading,
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
    /// A passage a paragraph quotes, set apart from its own words.
    Quote,
    Table,
    Row,
    Cell,
}

impl NodeKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Page => "page",
            Self::Section => "section",
            Self::Heading => "heading",
            Self::Footnote => "footnote",
            Self::Endnote => "endnote",
            Self::Prose => "prose",
            Self::List => "list",
            Self::ListItem => "list_item",
            Self::Quote => "quote",
            Self::Table => "table",
            Self::Row => "row",
            Self::Cell => "cell",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Derivation {
    Native,
    Heuristic,
}


/// Functional roles are independent of locators and structural object kinds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryRole {
    Text,
    Heading,
    /// The document's own title, set over all of it ("REASONS FOR JUDGMENT", "SECURITY AGREEMENT").
    Title,
    Caption,
    Instruction,
    Headnote,
    MarginalNote,
    Furniture,
    PageLabel,
    /// A transcript's printed line number: with the page label, the page and line a reader cites.
    LineLabel,
    /// A judgment's front matter, as its source marks it: the bench, counsel, a date, the
    /// docket number and the court.
    Judges,
    Counsel,
    Date,
    DocketNumber,
    Court,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructureNode {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<PrimaryRole>,
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
    /// A page's numbering, where a document prints more than one: a reporter's (`reporter-1`), a vendor's page scheme,
    /// the slip opinion's (`slip`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub series: String,
    /// Where a note is called: each mark in the text that points to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<ScalarRange>,
    /// Where the node stands in each of the document's page numberings: "226 La." at 123, running to 124.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<crate::opinion::Pin>,
    /// What a reader knows of the node past its kind and place, each under the reader's own name: a paragraph's list
    /// item or speaker.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
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
            series: String::new(),
            calls: Vec::new(),
            pins: Vec::new(),
            properties: BTreeMap::new(),
        }
    }

    /// The label as the document prints it, past the prefix its kind gives every label: "page702" is 702, "par12" 12,
    /// "fn3" 3; empty where there is none.
    pub fn printed_label(&self) -> &str {
        let prefix = match self.kind {
            NodeKind::Page => "page",
            NodeKind::Paragraph | NodeKind::Quote | NodeKind::Prose => "par",
            NodeKind::Footnote | NodeKind::Endnote => "fn",
            _ => "",
        };
        self.label.as_deref().map_or("", |label| label.strip_prefix(prefix).unwrap_or(label))
    }
}

/// The members of a JSON object as serde_json writes it, compact: each `"key":value`, in order.
fn object_members(object: &[u8]) -> Vec<&[u8]> {
    let mut members = Vec::new();
    let (mut depth, mut quoted, mut escaped, mut start) = (0usize, false, false, 1usize);
    for (at, &byte) in object.iter().enumerate() {
        if quoted {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 && at > start {
                    members.push(&object[start..at]);
                }
            }
            b',' if depth == 1 => {
                members.push(&object[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    members
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
pub enum ContinuationKind { Paragraph, Heading, Note, Table, List }

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
    /// A court opinion's readings that are no node of it (`opinion`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub opinion: Option<Box<crate::opinion::OpinionReading>>,
    pub diagnostics: Vec<StructureDiagnostic>,
}

impl DocumentStructure {
    pub fn project_scalar_parts(
        coordinates: &ScalarText<'_>,
        nodes: &mut [StructureNode],
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
            for call in &mut node.calls {
                utf16_range(call);
            }
        }
        for diagnostic in diagnostics {
            for range in &mut diagnostic.ranges {
                utf16_range(range);
            }
        }
    }

    /// A structure of `text` from one origin, complete, its digest taken, with no nodes yet: a reader sets on it what
    /// else it knows.
    pub fn new(document_id: String, provider: impl Into<String>, text: String, origin_id: &str) -> Self {
        use sha2::{Digest, Sha256};
        Self {
            schema_version: Cow::Borrowed(DOCUMENT_STRUCTURE_SCHEMA),
            document_id,
            offset_unit: Cow::Borrowed("utf16"),
            provider: provider.into(),
            url: None,
            doc_type: None,
            profile: None,
            revision: String::new(),
            text_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            text,
            rendered_text: None,
            source_sha256: None,
            scope: Scope::complete(),
            origins: vec![Origin { id: origin_id.to_owned() }],
            nodes: Vec::new(),
            source_fragments: Vec::new(),
            continuations: None,
            joins: Vec::new(),
            manuscript_edits: BTreeMap::new(),
            markup_elements: Vec::new(),
            markup_ancestors: Vec::new(),
            markup_sha256: None,
            cited_authorities: Vec::new(),
            definitions: Vec::new(),
            docx: None,
            selected_hypothesis: None,
            contents: None,
            cross_references: None,
            opinion: None,
            diagnostics: Vec::new(),
        }
    }

    /// A structure of nodes and diagnostics read in scalars, put in UTF-16, its revision taken.
    pub fn from_scalar_parts(
        document_id: String,
        provider: String,
        text: String,
        text_sha256: String,
        source_sha256: Option<String>,
        scope: Scope,
        origins: Vec<Origin>,
        mut nodes: Vec<StructureNode>,
        mut diagnostics: Vec<StructureDiagnostic>,
    ) -> Self {
        Self::project_scalar_parts(&ScalarText::new(&text), &mut nodes, &mut diagnostics);
        let mut structure = Self {
            text_sha256,
            source_sha256,
            scope,
            origins,
            nodes,
            diagnostics,
            ..Self::new(document_id, provider, text, "")
        };
        structure.refresh_revision().expect("Typed document structure is serializable");
        structure
    }

    /// A structure of nodes and diagnostics already in UTF-16, its revision taken.
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
        diagnostics: Vec<StructureDiagnostic>,
    ) -> Self {
        let mut structure = Self {
            url,
            doc_type,
            profile,
            text_sha256,
            source_sha256,
            scope,
            origins,
            nodes,
            diagnostics,
            ..Self::new(document_id, provider, text, "")
        };
        structure.refresh_revision().expect("Typed document structure is serializable");
        structure
    }

    /// Identity of the complete queryable representation, excluding the prior digest itself: the
    /// digest of the structure's JSON without its revision, its last member standing where the
    /// revision stood (as serde_json's value tree, keeping order, removes a key). The structure is
    /// written once and its top-level members reordered, never built as a value tree.
    pub fn refresh_revision(&mut self) -> Result<(), serde_json::Error> {
        use sha2::{Digest, Sha256};
        let prior = std::mem::take(&mut self.revision);
        let written = serde_json::to_vec(&*self);
        self.revision = prior;
        let written = written?;
        let mut members = object_members(&written);
        if let Some(at) = members.iter().position(|member| member.starts_with(br#""revision":"#)) {
            members.swap_remove(at);
        }
        let mut digest = Sha256::new();
        digest.update(b"{");
        for (k, member) in members.iter().enumerate() {
            if k > 0 {
                digest.update(b",");
            }
            digest.update(member);
        }
        digest.update(b"}");
        self.revision = format!("{:x}", digest.finalize());
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
