//! A court opinion's readings that are no node of it: the printings its pages belong to, its outcome and kind, and what the
//! record shows is missing. The structure's nodes hold the rest: its pages, paragraphs, notes, headings and parts, and as
//! spans (`NodeKind::Span`) its other readings, its headnotes and front lines among them (one the record holds apart from
//! the text stands at the text's start, holding none of it).

use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub record_gaps: Vec<RecordGap>,
    /// What the court did: `affirmed`, `reversed`, `remanded`, `vacated`, `dismissed`, `denied`, `granted`, `modified`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<String>,
    /// `court-opinion` or `attorney-general-opinion`, as the text reads, whatever the record's type says.
    #[serde(default)]
    pub document_kind: String,
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Pin {
    pub series: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    pub page: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
}
