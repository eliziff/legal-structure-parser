//! A brief read as its text units (its body paragraphs and its notes): the text the citation engine reads them as,
//! where each note stands in it, which body units a citation runs across, and its citations in reading order (a note's
//! after the body up to its call). Offsets count UTF-16 units, as the host's text does.

use serde::{Deserialize, Serialize};
use legal_structure::utf16_len as utf16;
use legal_structure_model::{AuthorityTextUnit, SourceMapping};
use std::collections::{HashMap, HashSet};

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

/// The brief's text, its units joined by a blank line, and its notes in reading order.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefText {
    pub text: String,
    pub notes: Vec<BriefNote>,
}

const UNKNOWN_NOTE: u64 = 0xffff_ffff;

fn starts(units: &[AuthorityTextUnit]) -> Vec<usize> {
    units.iter().scan(0, |offset, unit| Some(std::mem::replace(offset, *offset + utf16(&unit.text) + 2))).collect()
}

/// Each unit's items (by their offsets in it) in the brief's reading order, as (unit, item): a note follows the body up
/// to its call; a note no body unit calls keeps its place after the body.
fn reading_order(units: &[AuthorityTextUnit], offsets: &[Vec<usize>]) -> Vec<(usize, usize)> {
    let notes: HashMap<i64, usize> = units.iter().enumerate().filter(|(_, unit)| unit.kind == "footnote")
        .filter_map(|(index, unit)| unit.footnote_id.map(|id| (id as i64, index))).collect();
    let mut placed = vec![false; units.len()];
    let mut order = Vec::new();
    let place = |index: usize, placed: &mut [bool], order: &mut Vec<(usize, usize)>| {
        if !std::mem::replace(&mut placed[index], true) {
            order.extend((0..offsets[index].len()).map(|item| (index, item)));
        }
    };
    for (index, unit) in units.iter().enumerate().filter(|(_, unit)| unit.kind != "footnote") {
        placed[index] = true;
        let mut calls = unit.footnote_refs.clone();
        calls.sort_by_key(|&(_, offset)| offset);
        let mut next = 0;
        for (item, &offset) in offsets[index].iter().enumerate() {
            for &(id, _) in calls[next..].iter().take_while(|&&(_, at)| at <= offset) {
                if let Some(&note) = notes.get(&id) {
                    place(note, &mut placed, &mut order);
                }
                next += 1;
            }
            order.push((index, item));
        }
        for (id, _) in &calls[next..] {
            if let Some(&note) = notes.get(id) {
                place(note, &mut placed, &mut order);
            }
        }
    }
    (0..units.len()).for_each(|index| place(index, &mut placed, &mut order));
    order
}

pub fn brief_text(units: &[AuthorityTextUnit]) -> BriefText {
    let starts = starts(units);
    let order = reading_order(units, &vec![vec![0]; units.len()]);
    // Where each note's call stands in the text it annotates (a later call of the same note wins).
    let anchors = units.iter().zip(&starts).flat_map(|(unit, start)| unit.footnote_refs.iter()
        .map(move |&(id, offset)| (id, start + offset))).collect::<HashMap<_, _>>();
    let notes = order.into_iter().map(|(unit, _)| unit).filter_map(|index| {
        let unit = &units[index];
        let id = unit.footnote_id?;
        Some(BriefNote { number: unit.note_number.map_or(UNKNOWN_NOTE, u64::from), start: starts[index],
            end: starts[index] + utf16(&unit.text),
            sequence: if unit.note_number.is_none() { UNKNOWN_NOTE } else { unit.restart_sequence.unwrap_or(0) as u64 },
            anchor: anchors.get(&(id as i64)).copied() })
    }).collect();
    BriefText { text: units.iter().map(|unit| unit.text.as_str()).collect::<Vec<_>>().join("\n\n"), notes }
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
#[cfg_attr(feature = "binding-types", ts(optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct BriefCitation {
    pub index: usize,
    pub form: String,
    pub span: BriefSpan,
    pub full_span: BriefSpan,
    #[serde(default)]
    pub style: Option<BriefSpan>,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<BriefPinpoint>>", optional))]
    pub pinpoints: Vec<BriefPinpoint>,
}

/// The units as joined, where each starts in the brief's text, and the citations' indexes in reading order.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefCitationOrder {
    pub units: Vec<AuthorityTextUnit>,
    pub starts: Vec<usize>,
    pub order: Vec<usize>,
}

/// With `join_bodies` (a PDF's layout units can end inside a citation), each body unit a citation runs out of is joined
/// to the body unit it runs into (their texts by a blank line, their pages, calls and source words together); then each
/// citation is placed in the unit it starts in, and the citations are read in the brief's reading order.
pub fn brief_citation_order(units: &[AuthorityTextUnit], citations: &[BriefCitation], join_bodies: bool) -> BriefCitationOrder {
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
    let mut joined = Vec::<(AuthorityTextUnit, usize)>::new();
    for (index, (unit, &start)) in units.iter().zip(&starts).enumerate() {
        let Some((prior, from)) = joined.last_mut().filter(|_| index > 0 && joins.binary_search(&(index - 1)).is_ok())
        else {
            joined.push((unit.clone(), start));
            continue;
        };
        let shift = start - *from;
        prior.text = format!("{}\n\n{}", prior.text, unit.text);
        prior.page_numbers.extend(&unit.page_numbers);
        prior.page_numbers.sort_unstable();
        prior.page_numbers.dedup();
        prior.footnote_refs.extend(unit.footnote_refs.iter().map(|&(id, offset)| (id, shift + offset)));
        if let (Some(left), Some(right)) = (prior.source.as_mut(), &unit.source) {
            for (ids, more) in [(&mut left.node_ids, &right.node_ids), (&mut left.line_ids, &right.line_ids)] {
                let mut seen = HashSet::new();
                ids.extend(more.iter().cloned());
                ids.retain(|id| seen.insert(id.clone()));
            }
            left.ranges.extend(right.ranges.iter().cloned());
            left.mappings.extend(right.mappings.iter().map(|mapping| SourceMapping {
                start: mapping.start + shift, end: mapping.end + shift, ..mapping.clone() }));
        }
    }
    let (units, starts): (Vec<_>, Vec<_>) = joined.into_iter().unzip();
    // Each joined unit's citations (the unit each starts in), by index and offset.
    let (mut indexes, mut offsets) = (vec![Vec::new(); units.len()], vec![Vec::new(); units.len()]);
    let mut sorted = citations.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|citation| (citation.span.start, citation.index));
    let mut at = 0;
    for citation in sorted {
        while at + 1 < units.len() && citation.span.start >= starts[at] + utf16(&units[at].text) {
            at += 1;
        }
        if let Some(&start) = starts.get(at) {
            indexes[at].push(citation.index);
            offsets[at].push(citation.span.start - start.min(citation.span.start));
        }
    }
    let order = reading_order(&units, &offsets).into_iter().map(|(unit, item)| indexes[unit][item]).collect();
    BriefCitationOrder { units, starts, order }
}
