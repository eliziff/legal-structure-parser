use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberAnchor {
    pub number: String,
    pub paragraph_index: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberingDuplicate {
    pub number: String,
    pub previous_paragraph_index: usize,
    pub paragraph_index: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberingGap {
    pub previous_number: String,
    pub number: String,
    pub paragraph_index: usize,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberingResult {
    pub number_anchors: Vec<DocxNumberAnchor>,
    pub roman_article_anchors: Vec<DocxNumberAnchor>,
    pub duplicates: Vec<DocxNumberingDuplicate>,
    pub gaps: Vec<DocxNumberingGap>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocxCrossReferenceStatus {
    Resolved,
    SkippedExternal,
    MissingRomanArticle,
    MissingSibling { parent: String },
    MissingTopLevel,
    Abstained,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxCrossReference {
    pub paragraph_index: usize,
    pub subject: String,
    pub value: String,
    pub status: DocxCrossReferenceStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DocxAttachmentReferenceStatus {
    Resolved,
    Missing { included: Vec<String> },
    AbstainedNoAnchor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxAttachmentReference {
    pub paragraph_index: usize,
    pub label: String,
    pub id: String,
    pub subject: String,
    pub status: DocxAttachmentReferenceStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxStructureFacts {
    pub numbering: DocxNumberingResult,
    pub cross_references: Vec<DocxCrossReference>,
    pub attachments: Vec<DocxAttachmentReference>,
}
