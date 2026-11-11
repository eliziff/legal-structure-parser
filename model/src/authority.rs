//! An authority's text as its table of authorities reads it: each body paragraph and each note a unit, its text, where
//! its notes are called in it, and, for a PDF, where its words stand in the source.

use crate::ScalarRange;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct AuthorityTextUnit {
    /// "body:N" or "footnote:N".
    pub key: String,
    /// "body" or "footnote".
    #[cfg_attr(feature = "binding-types", ts(type = "\"body\" | \"footnote\""))]
    pub kind: String,
    pub ordinal: usize,
    /// A note's ordinal among the notes; none for a body unit.
    pub footnote_id: Option<usize>,
    /// The number a note prints, where it prints one, and the mark it prints otherwise (an author's "*").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub note_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub note_mark: Option<String>,
    /// The restart of the numbering a note counts in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub restart_sequence: Option<usize>,
    pub page_numbers: Vec<usize>,
    pub text: String,
    /// Each note called in the text: the note's ordinal (a DOCX note's id where it has none) and the call's UTF-16 offset.
    #[cfg_attr(feature = "binding-types", ts(type = "Array<[number, number]>"))]
    pub footnote_refs: Vec<(i64, usize)>,
    /// A DOCX body paragraph's place among the body's elements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub path: Option<Vec<usize>>,
    /// A DOCX note's links: each span of its text and the target it links to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<(usize, usize, String)>>", optional))]
    pub links: Vec<(usize, usize, String)>,
    /// Where a PDF unit's words stand in its source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub source: Option<AuthoritySource>,
}

/// A PDF unit's place in its document's source text (UTF-16): its nodes and lines, the source ranges its words come from,
/// and each run of its text that is a source line's text unchanged.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AuthoritySource {
    pub document_id: String,
    pub text_sha256: String,
    #[cfg_attr(feature = "binding-types", ts(type = r#""source_utf16""#))]
    pub coordinate_space: String,
    pub node_ids: Vec<String>,
    pub line_ids: Vec<String>,
    pub ranges: Vec<ScalarRange>,
    pub mappings: Vec<SourceMapping>,
}

/// A run of a unit's text (its UTF-16 `start`..`end`) that is the source's `source_start`..`source_end`, on line `line_id`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SourceMapping {
    pub start: usize,
    pub end: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub line_id: String,
}
