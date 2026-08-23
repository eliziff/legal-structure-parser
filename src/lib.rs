#[cfg(feature = "structure-inference")]
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{Display, Formatter};
#[cfg(feature = "structure-inference")]
use std::sync::OnceLock;

#[cfg(feature = "a2aj")]
mod a2aj;
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
mod amendments;
#[cfg(feature = "structure-inference")]
mod citator;
mod definitions;
#[cfg(feature = "document-query")]
mod document_block;
#[cfg(feature = "document-query")]
mod document_query;
mod docx_lint;
mod docx_numbering;
mod fingerprint;
mod instrument;
#[cfg(feature = "journal")]
mod journal;
mod locator;
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
mod native_markup;
mod numeric_sequence;
#[cfg(feature = "document-query")]
mod quote_verification;
mod tables;
mod text;
#[cfg(feature = "a2aj")]
pub use a2aj::{a2aj_document_structure, A2ajInput, A2ajSectionMap, A2ajSourceKind};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use amendments::*;
#[cfg(feature = "structure-inference")]
pub use citator::*;
pub use definitions::*;
#[cfg(feature = "document-query")]
pub use document_block::{
    DocumentBlock, DocumentKind, DocumentOrigin, DocumentProvider, DocumentType,
};
#[cfg(feature = "document-query")]
pub use document_query::*;
pub use docx_lint::*;
pub use docx_numbering::*;
pub use fingerprint::*;
pub use instrument::*;
#[cfg(feature = "journal")]
pub use journal::{
    journal_document_structure, journal_text_document_structure, pair_journal_footnotes,
    JournalFootnotePairing, JournalPageLabel, JournalPairNote,
};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use native_markup::{analyze_native_markup, NativeMarkupInput};
pub use numeric_sequence::*;
#[cfg(feature = "document-query")]
pub use quote_verification::*;
pub use tables::AuthoritativeTableCell;
pub(crate) use tables::AuthoritativeTables;
pub(crate) use text::javascript_whitespace;
pub use text::{normalize_javascript_whitespace, utf16_len, ScalarText};

pub const EVIDENCE_SCHEMA: &str = "legalpdf.structure-evidence.v1";
pub const DOCUMENT_STRUCTURE_SCHEMA: &str = "legalpdf.document-structure.v1";
const ENGINE_ORIGIN: &str = "legalpdf.structure-engine";

#[cfg(feature = "structure-inference")]
fn canadian_report_start(value: &str) -> Option<u32> {
    static REPORT: OnceLock<Regex> = OnceLock::new();
    REPORT
        .get_or_init(|| Regex::new(r"(?iu)\b(?:S\.?C\.?R\.?|R\.?C\.?S\.?)\s+(\d{1,4})\b").unwrap())
        .captures(value)
        .and_then(|capture| capture[1].parse().ok())
}

fn whole_document_coverage(
    end: usize,
    state: impl Fn(EvidenceKind) -> CoverageState,
) -> Vec<Coverage> {
    [
        EvidenceKind::Paragraph,
        EvidenceKind::Prose,
        EvidenceKind::Page,
        EvidenceKind::Section,
        EvidenceKind::Heading,
        EvidenceKind::Footnote,
        EvidenceKind::Endnote,
    ]
    .into_iter()
    .map(|kind| Coverage {
        kind,
        range: ScalarRange { start: 0, end },
        state: state(kind),
    })
    .collect()
}

#[derive(Debug)]
pub struct EngineError {
    pub code: &'static str,
    pub message: String,
}

impl EngineError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_evidence",
            message: message.into(),
        }
    }

    #[cfg(feature = "document-query")]
    fn source(message: impl Display) -> Self {
        Self {
            code: "invalid_source",
            message: message.to_string(),
        }
    }
}

impl Display for EngineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EngineError {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScalarRange {
    pub start: usize,
    pub end: usize,
}

impl ScalarRange {
    fn valid(self, length: usize) -> bool {
        self.start <= self.end && self.end <= length
    }
}

#[derive(Clone, Copy, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum EvidenceKind {
    Paragraph,
    Prose,
    Page,
    Section,
    Heading,
    Footnote,
    Endnote,
    List,
    Table,
    Row,
    Cell,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CoverageState {
    Absent,
    Augment,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Complete,
    Excerpt,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionProfile {
    CaseRootedComplete,
    CaseContiguousComplete,
    CaseLossy,
    Legislation,
    Instrument,
    Journal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: ScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt_of: Option<String>,
}

impl Scope {
    pub fn complete() -> Self {
        Self {
            kind: ScopeKind::Complete,
            excerpt_of: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeClaim {
    id: String,
    kind: EvidenceKind,
    label: Option<String>,
    aliases: Vec<String>,
    range: ScalarRange,
    origin_id: String,
    parent_label: Option<String>,
    anchor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Coverage {
    kind: EvidenceKind,
    range: ScalarRange,
    state: CoverageState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exclusion {
    range: ScalarRange,
    applies_to: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParagraphBreak {
    at: usize,
    origin_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentInput {
    schema_version: String,
    document_id: String,
    provider: String,
    #[cfg(feature = "document-query")]
    url: Option<String>,
    #[cfg(feature = "document-query")]
    doc_type: Option<DocumentType>,
    provider_revision: String,
    profile: DetectionProfile,
    report_start_page: Option<u32>,
    require_report_start: bool,
    allow_hyphenated_sections: bool,
    text: String,
    text_sha256: String,
    source_sha256: Option<String>,
    offset_unit: String,
    scope: Scope,
    origins: Vec<Origin>,
    native_claims: Vec<NativeClaim>,
    coverage: Vec<Coverage>,
    exclusions: Vec<Exclusion>,
    paragraph_breaks: Vec<ParagraphBreak>,
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn nonempty(values: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    values.into_iter().all(|value| !value.as_ref().is_empty())
}

impl EvidenceKind {
    fn name(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Prose => "prose",
            Self::Page => "page",
            Self::Section => "section",
            Self::Heading => "heading",
            Self::Footnote => "footnote",
            Self::Endnote => "endnote",
            Self::List => "list",
            Self::Table => "table",
            Self::Row => "row",
            Self::Cell => "cell",
        }
    }
}

impl DocumentInput {
    fn validate(&self) -> Result<(), EngineError> {
        let length = self.text.chars().count();
        if self.schema_version != EVIDENCE_SCHEMA
            || self.offset_unit != "unicode-scalar"
            || !nonempty([
                &self.document_id,
                &self.provider,
                &self.provider_revision,
                &self.text_sha256,
            ])
            || !hash(&self.text_sha256)
            || format!("{:x}", Sha256::digest(self.text.as_bytes())) != self.text_sha256
            || self
                .source_sha256
                .as_deref()
                .is_some_and(|value| !hash(value))
            || match self.scope.kind {
                ScopeKind::Complete => self.scope.excerpt_of.is_some(),
                ScopeKind::Excerpt => self.scope.excerpt_of.as_deref().is_none_or(str::is_empty),
            }
        {
            return Err(EngineError::invalid("invalid evidence identity or schema"));
        }
        if self.profile != DetectionProfile::Legislation && self.allow_hyphenated_sections {
            return Err(EngineError::invalid(
                "hyphenated-section option requires legislation profile",
            ));
        }
        if matches!(
            self.profile,
            DetectionProfile::CaseRootedComplete | DetectionProfile::CaseContiguousComplete
        ) && self.scope.kind != ScopeKind::Complete
        {
            return Err(EngineError::invalid(
                "complete case profile requires complete document scope",
            ));
        }
        if matches!(
            self.profile,
            DetectionProfile::Legislation
                | DetectionProfile::Instrument
                | DetectionProfile::Journal
        ) && (self.report_start_page.is_some() || self.require_report_start)
        {
            return Err(EngineError::invalid(
                "report-page options require a case profile",
            ));
        }
        let origins = self
            .origins
            .iter()
            .map(|value| value.id.as_str())
            .collect::<HashSet<_>>();
        if origins.len() != self.origins.len() || origins.contains("") {
            return Err(EngineError::invalid("origins are invalid or duplicated"));
        }
        let claims = self
            .native_claims
            .iter()
            .map(|value| value.id.as_str())
            .collect::<HashSet<_>>();
        if claims.len() != self.native_claims.len()
            || self.native_claims.iter().any(|value| {
                value.id.is_empty()
                    || !value.range.valid(length)
                    || !origins.contains(value.origin_id.as_str())
                    || !nonempty(value.aliases.iter())
                    || value.label.as_deref().is_some_and(str::is_empty)
                    || value.anchor.as_deref().is_some_and(str::is_empty)
            })
        {
            return Err(EngineError::invalid(
                "native claims are invalid or duplicated",
            ));
        }
        let mut coverage = BTreeMap::<EvidenceKind, Vec<ScalarRange>>::new();
        for value in &self.coverage {
            if !value.range.valid(length) {
                return Err(EngineError::invalid("coverage is invalid"));
            }
            coverage.entry(value.kind).or_default().push(value.range);
        }
        for kind in [
            EvidenceKind::Paragraph,
            EvidenceKind::Prose,
            EvidenceKind::Page,
            EvidenceKind::Section,
            EvidenceKind::Heading,
            EvidenceKind::Footnote,
            EvidenceKind::Endnote,
        ] {
            let Some(rows) = coverage.get_mut(&kind) else {
                return Err(EngineError::invalid("coverage kind is missing"));
            };
            rows.sort_by_key(|value| value.start);
            let mut cursor = 0;
            for range in rows {
                if range.start != cursor {
                    return Err(EngineError::invalid("coverage has a gap or overlap"));
                }
                cursor = range.end;
            }
            if cursor != length {
                return Err(EngineError::invalid("coverage does not span text"));
            }
        }
        if self.exclusions.iter().any(|value| {
            !value.range.valid(length)
                || value.applies_to.is_empty()
                || !nonempty(value.applies_to.iter())
        }) || self
            .paragraph_breaks
            .iter()
            .any(|value| value.at > length || !origins.contains(value.origin_id.as_str()))
        {
            return Err(EngineError::invalid(
                "exclusion or paragraph break is invalid",
            ));
        }
        Ok(())
    }

    fn clip_inference(&self, kind: EvidenceKind, range: ScalarRange) -> Option<ScalarRange> {
        let mut end = range.end;
        for value in self
            .coverage
            .iter()
            .filter(|value| value.kind == kind && value.state == CoverageState::Complete)
        {
            if value.range.start <= range.start && range.start < value.range.end {
                return None;
            }
            if value.range.start > range.start {
                end = end.min(value.range.start);
            }
        }
        for value in self
            .exclusions
            .iter()
            .filter(|value| value.applies_to.iter().any(|name| name == kind.name()))
        {
            if value.range.start <= range.start && range.start < value.range.end {
                return None;
            }
            if value.range.start > range.start {
                end = end.min(value.range.start);
            }
        }
        (end > range.start).then_some(ScalarRange {
            start: range.start,
            end,
        })
    }

    fn needs_inference(&self) -> bool {
        self.coverage
            .iter()
            .any(|value| value.state != CoverageState::Complete)
    }
}

impl TryFrom<Value> for DocumentInput {
    type Error = EngineError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let evidence: DocumentInput = serde_json::from_value(value)
            .map_err(|error| EngineError::invalid(error.to_string()))?;
        evidence.validate()?;
        Ok(evidence)
    }
}

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
    fn evidence(self) -> EvidenceKind {
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
    fn name(self) -> &'static str {
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
    #[serde(skip)]
    pub rendered_range: Option<ScalarRange>,
    pub origin_id: String,
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
    pub(crate) fn new(
        id: String,
        kind: NodeKind,
        range: ScalarRange,
        origin_id: impl Into<String>,
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

pub(crate) fn public_structure_label(value: &str) -> String {
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
    result
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NoteReference {
    pub range: ScalarRange,
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
    pub schema_version: String,
    pub document_id: String,
    pub offset_unit: String,
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
    #[serde(skip)]
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
            schema_version: DOCUMENT_STRUCTURE_SCHEMA.to_owned(),
            document_id,
            offset_unit: "utf16".to_owned(),
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

    pub fn query_range(&self, node: &StructureNode) -> ScalarRange {
        node.rendered_range.unwrap_or(node.range)
    }
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateGrammar {
    Numeric,
    Hierarchy,
    Enumerator,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructureMarkerCandidate {
    pub id: String,
    pub range: ScalarRange,
    pub marker_range: ScalarRange,
    pub label: String,
    pub grammar_value: String,
    pub parent_candidate_id: Option<String>,
    pub level: usize,
    pub content_start: usize,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructureCandidateRun {
    pub id: String,
    pub grammar: CandidateGrammar,
    pub range: ScalarRange,
    pub rooted: bool,
    pub consecutive: bool,
    pub markers: Vec<StructureMarkerCandidate>,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateEvidenceV2 {
    pub candidate_id: String,
    pub page_indexes: Vec<usize>,
    pub line_ids: Vec<String>,
    pub observations: Vec<CandidateObservationV2>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateObservationV2 {
    BodyProseFlow,
    SectionHeading,
    ListItemLayout,
    CrossReference,
    Furniture,
    TableOrForm,
    ContentsRow,
    IndexRow,
    TranscriptLineNumber,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextAnchorV2 {
    pub range: ScalarRange,
    pub page_index: usize,
    pub line_id: String,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteBodyV2 {
    pub range: ScalarRange,
    pub page_indexes: Vec<usize>,
    pub line_ids: Vec<String>,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotePairClaimV2 {
    pub pair_id: String,
    pub kind: NoteKindV2,
    pub label: TextAnchorV2,
    pub body: NoteBodyV2,
    pub references: Vec<TextAnchorV2>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionRuleV2 {
    RootedNumericProse,
    HierarchySection,
    ListItemLayout,
    PairedNote,
    DirectExclusion,
    ConflictingRoles,
    InsufficientEvidence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolutionProofV2 {
    pub rule: ResolutionRuleV2,
    pub observations: Vec<CandidateObservationV2>,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedRole {
    NumberedParagraph,
    Section,
    ListItem,
}

#[cfg(feature = "structure-inference")]
impl ResolvedRole {
    pub fn node_kind(self) -> NodeKind {
        match self {
            Self::NumberedParagraph => NodeKind::Paragraph,
            Self::Section => NodeKind::Section,
            Self::ListItem => NodeKind::ListItem,
        }
    }
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCandidate {
    pub candidate: StructureMarkerCandidate,
    pub role: Option<ResolvedRole>,
    pub proof: ResolutionProofV2,
    pub page_indexes: Vec<usize>,
    pub line_ids: Vec<String>,
}

#[derive(Clone)]
struct Block {
    kind: NodeKind,
    range: ScalarRange,
    label: Option<String>,
    aliases: Vec<String>,
    parent_label: Option<String>,
    content_start: Option<usize>,
    diagnostic: Option<&'static str>,
    source: Derivation,
    origin_id: &'static str,
}

impl Block {
    fn labelled(kind: NodeKind, label: String, start: usize, end: usize) -> Self {
        Self {
            kind,
            range: ScalarRange { start, end },
            label: Some(label),
            aliases: Vec::new(),
            parent_label: None,
            content_start: None,
            diagnostic: None,
            source: Derivation::Heuristic,
            origin_id: ENGINE_ORIGIN,
        }
    }
}

#[cfg(feature = "structure-inference")]
mod inference;

#[cfg(feature = "structure-inference")]
mod candidates;
mod derive;

#[cfg(all(feature = "structure-inference", test))]
pub(crate) use candidates::resolve_structure_candidates;
#[cfg(feature = "structure-inference")]
pub use candidates::{detect_structure_candidate_runs, resolve_structure_graph};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use derive::derive_document_structure;
#[cfg(any(feature = "journal", test))]
pub(crate) use derive::derive_native_structure_evidence;
#[cfg(all(feature = "structure-inference", test))]
pub(crate) use derive::derive_structure_evidence;
#[cfg(test)]
mod tests;
