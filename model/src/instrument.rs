use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsEntry {
    pub label: String,
    pub display: String,
    pub heading: String,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_label: Option<String>,
    pub page: Option<u32>,
    pub contents_line_start: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsOutline {
    pub entries: Vec<InstrumentContentsEntry>,
    pub region_start: usize,
    pub region_end: usize,
    pub pages_cited: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentContentsRefusal {
    NoContentsMarker,
    NoContentsEntries,
    TooFewContentsEntries,
    ContentsWithoutPageNumbers,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InstrumentContentsReading {
    pub outline: Option<InstrumentContentsOutline>,
    pub refusal: Option<InstrumentContentsRefusal>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentCrossReferenceStatus {
    Resolved,
    External,
    Unresolved,
    Abstained,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentCrossReferenceReason {
    ExternalInstrument,
    DocumentAbstained,
    NoContainingSection,
    AmbiguousLabel,
    DepthNotNumbered,
    NoSuchProvision,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceEdge {
    pub source_start: usize,
    pub source_end: usize,
    pub source_label: Option<String>,
    pub raw: String,
    pub raw_label: String,
    pub normalized_locator: String,
    pub target_label: Option<String>,
    pub target_start: Option<usize>,
    pub target_end: Option<usize>,
    pub status: InstrumentCrossReferenceStatus,
    pub self_loop: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<InstrumentCrossReferenceReason>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceCounts {
    pub detected: usize,
    pub resolved: usize,
    pub external: usize,
    pub unresolved: usize,
    pub abstained: usize,
    pub self_loops: usize,
    pub integrity: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceGraph {
    pub edges: Vec<InstrumentCrossReferenceEdge>,
    pub document_abstained: bool,
    pub note: Option<String>,
    pub counts: InstrumentCrossReferenceCounts,
}
