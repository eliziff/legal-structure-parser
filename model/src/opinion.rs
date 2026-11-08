//! A court opinion's readings that are no node of it: the printings its pages belong to, where each passage stands in each,
//! its outcome, the quotations, terms and documents it carries, and what the record shows is missing. Ranges are UTF-16 in
//! the structure's text. The structure's nodes hold its pages, paragraphs, notes, headings, headnotes and parts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ScalarRange;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct OpinionReading {
    /// The rendition the text is read from: `html_with_citations.html`, `plain_text.txt`.
    pub rendition: String,
    /// The case's citations, CourtListener's table of them: each reporter it is printed in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<OpinionCitation>,
    /// Each page numbering and the printing it belongs to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_series: Vec<PageSeries>,
    /// Each node's place in every numbering, by node id: a paragraph's, a heading's, a note's call's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, Vec<Pin>>,
    /// Headnotes the opinion's text does not carry: the cluster's, with their words alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headnotes: Vec<OpinionHeadnote>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headnote_marks: Vec<HeadnoteMark>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<OpinionBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub front: Vec<FrontLine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub furniture: Vec<FurnitureLine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_calls: Vec<UnmatchedCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<OpinionTable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defined_terms: Vec<DefinedTerm>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotes: Vec<OpinionQuote>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruments: Vec<OpinionInstrument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub record_gaps: Vec<RecordGap>,
    /// What the court did: `affirmed`, `reversed`, `remanded`, `vacated`, `dismissed`, `denied`, `granted`, `modified`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<String>,
    /// `court-opinion` or `attorney-general-opinion`, as the text reads, whatever the record's type says.
    #[serde(default)]
    pub document_kind: String,
    /// Renditions of this record that hold another opinion's text, and were not read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_opinion: Vec<String>,
    /// The parse's own measures and self-checks, for the census: no reading of the opinion.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub census: serde_json::Value,
}

/// One headnote, syllabus point or summary paragraph. Its place is in the text where the opinion carries it; the
/// cluster's have only their words.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpinionHeadnote {
    /// `headnotes`, `syllabus`, `summary`, `arguments`, `history`, `posture`, `procedural_history`, `cross_reference`,
    /// `correction`, `seealso`, `note` (a reporter's or editor's), `held`, `catchword`, `topics`, `topic_index`, `papers`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// The point's subject, where it leads with one: "APPEAL — Time for Perfecting. ..."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ScalarRange>,
}

/// A headnote's number printed in the opinion, "[3]" or "[5, 6]", at the passage it keys.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HeadnoteMark {
    pub numbers: Vec<u32>,
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A block counter a source sets, `<div class="num"><span class="num">14</span>`: no number the court printed, but the only one
/// some texts carry, and it may stand on a paragraph, a title or a note. `source`: `marked` in this text, `other-rendition`
/// placed by the words after it, `reckoned` between its neighbours where those words are not in this text.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpinionBlock {
    pub label: String,
    pub start: usize,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<Pin>,
}

/// A line of the case's front matter: `court`, `docket`, `date`, `judges`, `counsel`, `caption`, `author`, `title`,
/// `citation` (the case's own), `disposition`, `case_name`, `case_name_full`, `date_filed`, `other_dates`, `nature_of_suit`, `lower_court`,
/// `publication`, `subject`, `addressee`, `contents`, `closing`, `joinder`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FrontLine {
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ScalarRange>,
}

/// A note's call with no note anywhere in the record: the mark stands, its note was never reported.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UnmatchedCall {
    pub label: String,
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A short name the opinion gives something: `("Debtor")`, `Administrative Procedure Act (APA)`, `"Lease" means`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DefinedTerm {
    pub term: String,
    /// `parenthetical`, `acronym` or `means`.
    pub form: String,
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// An editor's mark in a quotation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuoteMark {
    /// `insertion`, `sic`, `omission`, `omitted-note`, or `emphasis` for the words that say so after it.
    pub kind: String,
    pub start: usize,
    pub end: usize,
}

/// A quotation, set inline or as a block, with the marks an editor made in it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpinionQuote {
    pub block: bool,
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<QuoteMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A document the opinion reproduces: the quoted passages a lead-in names, "the will provides as follows:".
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpinionInstrument {
    /// `will`, `contract`, `jury-charge`, `verdict`, `testimony`, `letter`, `order`, `statute`, `passage` and the like.
    pub kind: String,
    pub lead_in: String,
    pub start: usize,
    pub end: usize,
}

/// Something the record itself shows is not all here.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecordGap {
    /// `separate-opinion-not-here`, `opens-midway`, `facts-not-here`, `image-not-here`, `later-ruling-to-come`, `attachment-named`.
    pub kind: String,
    pub evidence: String,
}

/// One reporter's printing of the case.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpinionCitation {
    pub volume: String,
    pub reporter: String,
    pub page: String,
    /// CourtListener's type: 1 federal, 2 state, 3 regional, 4 specialty, 5 early Supreme Court, 6 Lexis, 7 Westlaw, 8 neutral, 9 journal.
    pub kind: String,
}

impl OpinionCitation {
    pub fn printed(&self) -> String {
        format!("{} {} {}", self.volume, self.reporter, self.page)
    }
}

/// A numbering of pages and the printing it belongs to: which reporter's pages these are.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PageSeries {
    pub series: String,
    pub citation: String,
    /// Volume and reporter, as a pin is cited: "226 La.".
    pub reporter: String,
    /// Other citations that open on the same page, where the record cannot say which of them it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
}

/// Where a passage falls in one numbering: "226 La." at 123, running to 124.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Pin {
    pub series: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    pub page: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
}

/// A data table in the opinion, its rows out of the paragraphs.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpinionTable {
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A line the reading text leaves out of its flow: a page's running title, number or stamp, and where it stood.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FurnitureLine {
    pub text: String,
    pub at: usize,
}
