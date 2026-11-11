use crate::{
    DefinedTerm, DetectionProfile, DocxStructureFacts, InstrumentContentsReading,
    InstrumentCrossReferenceGraph, Origin, ScalarRange, Scope,
    DOCUMENT_STRUCTURE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeMap};

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
    /// A stretch of the text a reader names that is no block of it: a headnote or a line of the front matter, a defined
    /// term, a quotation and an editor's mark in it, a mark keying a headnote. Its `grammar` says which reading it is.
    Span,
}

impl NodeKind {
    pub const ALL: [Self; 14] = [
        Self::Paragraph, Self::Page, Self::Section, Self::Heading, Self::Footnote, Self::Endnote, Self::Prose, Self::List,
        Self::ListItem, Self::Quote, Self::Table, Self::Row, Self::Cell, Self::Span,
    ];

    /// The kind `name` names: `name`'s inverse.
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

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
            Self::Span => "span",
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
    /// A party's role in a judgment's caption ("Appellant", "Respondents").
    Party,
    /// A line of the front matter that names what it gives ("DOCKET: C69420").
    Labelled,
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
    /// Where the node reads in the query text, as assembly writes it; in a rendered text (`rendered_text`), none where
    /// it reads nowhere there.
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
    /// A page's numbering, where a document prints more than one: a reporter's (`reporter-1`), a vendor's page scheme,
    /// the slip opinion's (`slip`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub series: String,
    /// Where a note is called: each mark in the text that points to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<ScalarRange>,
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
            series: String::new(),
            calls: Vec::new(),
            properties: BTreeMap::new(),
        }
    }

    /// The node's place as a link names it, its kind's mark before its label: "par12", "page702", "fn3", a provision's
    /// by its locator's kind ("sec5(2)", "part2", "schedb").
    pub fn fragment(&self) -> Option<String> {
        let mark = match self.kind {
            NodeKind::Page => "page",
            NodeKind::Paragraph | NodeKind::Quote | NodeKind::Prose => "par",
            NodeKind::Footnote | NodeKind::Endnote => "fn",
            NodeKind::Section => provision_mark(self.locator_kind.as_deref()),
            _ => "",
        };
        self.label.as_deref().map(|label| format!("{mark}{label}"))
    }

    /// The names a reader's own references give the node: its label and each of its aliases, a provision's under its
    /// kind's mark ("sec5(2)", "part2"), as its parents and its claims name it.
    pub fn places(&self) -> impl Iterator<Item = String> + '_ {
        let mark = if self.kind == NodeKind::Section { provision_mark(self.locator_kind.as_deref()) } else { "" };
        self.label.iter().chain(self.aliases.iter().flatten()).map(move |name| format!("{mark}{name}"))
    }

    /// Whether the node prints its label where it stands: its marker holds text ("[12]" before a paragraph), not a
    /// number its reader inferred.
    pub fn prints_label(&self) -> bool {
        self.marker_range.is_some_and(|range| range.start < range.end)
    }
}

/// The mark a provision's place carries in a link, by its locator's kind: "sec" for a section, "art", "part", "div",
/// "sched", "exh", "annex" and "app" for an article, part, division, schedule, exhibit, annex and appendix; a clause the
/// grammar reads by its number alone, or a provision of no kind, carries none.
pub fn provision_mark(kind: Option<&str>) -> &'static str {
    match kind {
        Some("section" | "subsection") => "sec",
        Some("article") => "art",
        Some("part") => "part",
        Some("division") => "div",
        Some("schedule") => "sched",
        Some("exhibit") => "exh",
        Some("annex") => "annex",
        Some("appendix") => "app",
        _ => "",
    }
}

/// A node as a reader takes it, with what the structure reads of it: the spans of its query text the node holds and
/// their envelope (`query_ranges`, `query_range`), its link fragment, and whether it prints its label where it stands.
#[derive(Clone, Debug, Serialize)]
pub struct NodeReading<'a> {
    pub node: &'a StructureNode,
    pub ranges: Cow<'a, [ScalarRange]>,
    pub span: ScalarRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
    pub prints_label: bool,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CitedAuthority {
    pub citation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
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

impl MarkupElement {
    /// Whether the element's class list names `class`, as CSS matches a class.
    pub fn has_class(&self, class: &str) -> bool {
        self.classes().any(|value| value == class)
    }

    /// The element's classes, as its class attribute lists them.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attributes.get("class").map_or("", String::as_str).split_ascii_whitespace()
    }

    /// Whether the element is the source tag `tag`, whatever case the markup spells it in.
    pub fn is(&self, tag: &str) -> bool {
        self.tag.eq_ignore_ascii_case(tag)
    }
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
}

impl DocumentStructure {
    /// A node of the structure as a reader takes it (`NodeReading`).
    pub fn reading<'a>(&self, node: &'a StructureNode) -> NodeReading<'a> {
        NodeReading { node, ranges: self.query_ranges(node), span: self.query_range(node), fragment: node.fragment(),
            prints_label: node.prints_label() }
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
        }
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
        if !node.rendered_ranges.is_empty() || self.rendered_text.is_some() {
            return Cow::Borrowed(&node.rendered_ranges);
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

    /// The envelope of the node's query ranges; a node that reads nowhere in a rendered text stands at its start.
    pub fn query_range(&self, node: &StructureNode) -> ScalarRange {
        let ranges = self.query_ranges(node);
        match (ranges.iter().map(|range| range.start).min(), ranges.iter().map(|range| range.end).max()) {
            (Some(start), Some(end)) => ScalarRange { start, end },
            _ => ScalarRange { start: 0, end: 0 },
        }
    }
}
