use crate::{text::ScalarText, ScalarRange};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinitionOccurrence {
    pub range: ScalarRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub source_paragraph_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_artifact_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinedTerm {
    pub term: String,
    pub definitions: Vec<DefinitionOccurrence>,
    pub uses: Vec<DefinitionOccurrence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinitionsResult {
    pub terms: Vec<DefinedTerm>,
}

impl DefinitionOccurrence {
    pub fn at(&self, document: &ScalarText<'_>, start: usize, end: usize) -> Self {
        let mut hit = self.clone();
        (hit.range.start, hit.range.end) = (
            document.utf16_at_byte(start).unwrap(),
            document.utf16_at_byte(end).unwrap(),
        );
        hit
    }
}
