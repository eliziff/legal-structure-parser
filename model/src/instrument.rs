use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsEntry {
    /// The entry's label as the provision it names holds it ("5.2", "4", "b"), and that provision's kind ("section",
    /// "part", "schedule", ...).
    pub label: String,
    pub locator_kind: String,
    pub display: String,
    pub heading: String,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub parent_label: Option<String>,
    pub page: Option<u32>,
    pub contents_line_start: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsOutline {
    pub entries: Vec<InstrumentContentsEntry>,
    pub region_start: usize,
    pub region_end: usize,
    pub pages_cited: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum InstrumentContentsRefusal {
    NoContentsMarker,
    NoContentsEntries,
    TooFewContentsEntries,
    ContentsWithoutPageNumbers,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct InstrumentContentsReading {
    pub outline: Option<InstrumentContentsOutline>,
    pub refusal: Option<InstrumentContentsRefusal>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum InstrumentCrossReferenceStatus {
    Resolved,
    External,
    Unresolved,
    Abstained,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
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
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub reason: Option<InstrumentCrossReferenceReason>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceGraph {
    pub edges: Vec<InstrumentCrossReferenceEdge>,
    pub document_abstained: bool,
    pub note: Option<String>,
    pub counts: InstrumentCrossReferenceCounts,
}
