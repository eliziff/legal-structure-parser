//! A brief read as its text units (its body paragraphs and its notes): the text the citation engine reads them as,
//! where each note stands in it, which body units a citation runs across, and its citations in reading order (a note's
//! after the body up to its call). Offsets count UTF-16 units, as the host's text does.

use crate::reading_order::{document_reading_order, ReadingOrderUnit};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A unit of a brief as the parsers give it (`AuthorityTextUnit`), the fields read of it.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefUnit {
    pub kind: String,
    pub text: String,
    pub footnote_id: Option<usize>,
    #[serde(default)]
    pub note_number: Option<u64>,
    #[serde(default)]
    pub restart_sequence: Option<u64>,
    #[serde(default)]
    pub footnote_refs: Vec<(usize, usize)>,
}

/// A note as the citation engine reads a brief's notes: its printed number (none: u32::MAX), its span of the text,
/// its numbering sequence and where its call stands.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefNote {
    pub number: u64,
    pub start: usize,
    pub end: usize,
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub anchor: Option<usize>,
}

/// The brief's text, its units joined by a blank line, where each unit starts in it, and its notes in reading order.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefText {
    pub text: String,
    pub starts: Vec<usize>,
    pub notes: Vec<BriefNote>,
}

const UNKNOWN_NOTE: u64 = 0xffff_ffff;

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

fn starts(units: &[BriefUnit]) -> Vec<usize> {
    let mut offset = 0;
    units.iter().map(|unit| {
        let start = offset;
        offset += utf16(&unit.text) + 2;
        start
    }).collect()
}

pub fn brief_text(units: &[BriefUnit]) -> BriefText {
    let starts = starts(units);
    let order = document_reading_order(&units.iter().map(|unit| ReadingOrderUnit { kind: unit.kind.clone(),
        footnote_id: unit.footnote_id, footnote_refs: unit.footnote_refs.clone(), item_offsets: vec![0] })
        .collect::<Vec<_>>());
    // Where each note's call stands in the text it annotates (a later call of the same note wins).
    let anchors = units.iter().zip(&starts).flat_map(|(unit, start)| unit.footnote_refs.iter()
        .map(move |&(id, offset)| (id, start + offset))).collect::<HashMap<_, _>>();
    let notes = order.into_iter().map(|(unit, _)| unit).filter_map(|index| {
        let unit = &units[index];
        let id = unit.footnote_id?;
        Some(BriefNote { number: unit.note_number.unwrap_or(UNKNOWN_NOTE), start: starts[index],
            end: starts[index] + utf16(&unit.text),
            sequence: if unit.note_number.is_none() { UNKNOWN_NOTE } else { unit.restart_sequence.unwrap_or(0) },
            anchor: anchors.get(&id).copied() })
    }).collect();
    BriefText { text: units.iter().map(|unit| unit.text.as_str()).collect::<Vec<_>>().join("\n\n"), starts, notes }
}

#[derive(Deserialize, Clone, Copy)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefPinpoint {
    pub span: BriefSpan,
}

/// A citation the engine extracted from the brief's text, the fields read of it.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BriefCitation {
    pub index: usize,
    pub form: String,
    pub span: BriefSpan,
    pub full_span: BriefSpan,
    #[serde(default)]
    pub style: Option<BriefSpan>,
    #[serde(default)]
    pub pinpoints: Vec<BriefPinpoint>,
}

/// The body units a citation runs across, each joined to the next (`joins`: the first of each pair), and the
/// citations' indexes in reading order.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefCitationOrder {
    pub joins: Vec<usize>,
    pub order: Vec<usize>,
}

/// With `join_bodies` (a PDF's layout units can end inside a citation), each body unit a citation runs out of is joined
/// to the body unit it runs into; then each citation is placed in the unit it starts in, and the citations are read in
/// the brief's reading order.
pub fn brief_citation_order(units: &[BriefUnit], citations: &[BriefCitation], join_bodies: bool) -> BriefCitationOrder {
    let starts = starts(units);
    let ranges = units.iter().zip(&starts).map(|(unit, &start)| (start, start + utf16(&unit.text))).collect::<Vec<_>>();
    let mut joins = Vec::new();
    if join_bodies {
        for citation in citations.iter().filter(|citation| citation.form != "unknown") {
            let spans = [citation.full_span, citation.span].into_iter().chain(citation.style)
                .chain(citation.pinpoints.iter().map(|pinpoint| pinpoint.span)).collect::<Vec<_>>();
            let start = spans.iter().map(|span| span.start).min().unwrap_or(0);
            let end = spans.iter().map(|span| span.end).max().unwrap_or(0);
            let Some(mut index) = ranges.iter().position(|range| range.1 > start) else { continue };
            while index + 1 < ranges.len() && ranges[index + 1].0 < end {
                if units[index].kind == "body" && units[index + 1].kind == "body" && !joins.contains(&index) {
                    joins.push(index);
                }
                index += 1;
            }
        }
        joins.sort_unstable();
    }
    // The units as joined: each its first unit, its span and its notes' calls, offsets from its start.
    let mut merged: Vec<(usize, (usize, usize), Vec<(usize, usize)>)> = Vec::new();
    for (index, (unit, &range)) in units.iter().zip(&ranges).enumerate() {
        match merged.last_mut() {
            Some((_, span, refs)) if index > 0 && joins.binary_search(&(index - 1)).is_ok() => {
                refs.extend(unit.footnote_refs.iter().map(|&(id, offset)| (id, range.0 - span.0 + offset)));
                span.1 = range.1;
            }
            _ => merged.push((index, range, unit.footnote_refs.clone())),
        }
    }
    let mut items = vec![Vec::<(usize, usize)>::new(); merged.len()];
    let mut sorted = citations.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|citation| (citation.span.start, citation.index));
    let mut at = 0;
    for citation in sorted {
        while at + 1 < merged.len() && citation.span.start >= merged[at].1 .1 {
            at += 1;
        }
        if let Some((_, span, _)) = merged.get(at) {
            items[at].push((citation.index, citation.span.start - span.0.min(citation.span.start)));
        }
    }
    let order = document_reading_order(&merged.iter().zip(&items).map(|((first, _, refs), items)| ReadingOrderUnit {
        kind: units[*first].kind.clone(), footnote_id: units[*first].footnote_id, footnote_refs: refs.clone(),
        item_offsets: items.iter().map(|(_, offset)| *offset).collect() }).collect::<Vec<_>>());
    BriefCitationOrder { joins, order: order.into_iter().map(|(unit, item)| items[unit][item].0).collect() }
}
