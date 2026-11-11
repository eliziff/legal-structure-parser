//! A brief read as its text units (its body paragraphs and its notes): the text the citation engine reads them as,
//! where each note stands in it, which body units a citation runs across, its citations in reading order (a note's
//! after the body up to its call), and the authorities they cite. Offsets count UTF-16 units, as the host's text does.

use serde::{Deserialize, Serialize};
use legal_structure::citations::aliases::SourceAliasGroup;
use legal_structure::citations::api::{self, ResolveResponse};
use legal_structure::citations::authorities::{authorities, AuthoritiesRequest, AuthorityReading};
use legal_structure::citations::source::{references, ReferencePart, SourcePart};
use legal_structure::citations::{Authority, Citation, CitationStyle, Form, Format, NoteRange, OffsetUnit, Options, SplitTier, SupraMode};
use legal_structure::utf16_len as utf16;
use legal_structure_model::{AuthorityTextUnit, SourceMapping};
use std::collections::{HashMap, HashSet};

/// The brief's text, its units joined by a blank line, and its notes in reading order as the citation engine reads a
/// brief's notes: each note's printed number (none: u32::MAX), its span of the text, its numbering sequence and where
/// its call stands.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefText {
    pub text: String,
    pub notes: Vec<NoteRange>,
}

const UNKNOWN_NOTE: u32 = 0xffff_ffff;

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
        Some(NoteRange { number: unit.note_number.unwrap_or(UNKNOWN_NOTE), start: starts[index],
            end: starts[index] + utf16(&unit.text),
            sequence: if unit.note_number.is_none() { UNKNOWN_NOTE } else { unit.restart_sequence.unwrap_or(0) as u32 },
            anchor: anchors.get(&(id as i64)).copied() })
    }).collect();
    BriefText { text: units.iter().map(|unit| unit.text.as_str()).collect::<Vec<_>>().join("\n\n"), notes }
}

/// The units as joined, where each starts in the brief's text, and the citations' indexes in reading order.
struct BriefCitationOrder {
    units: Vec<AuthorityTextUnit>,
    starts: Vec<usize>,
    order: Vec<usize>,
}

/// With `join_bodies` (a PDF's layout units can end inside a citation), each body unit a citation runs out of is joined
/// to the body unit it runs into (their texts by a blank line, their pages, calls and source words together); then each
/// citation is placed in the unit it starts in, and the citations are read in the brief's reading order.
fn brief_citation_order(units: &[AuthorityTextUnit], citations: &[Citation], join_bodies: bool) -> BriefCitationOrder {
    let starts = starts(units);
    let ranges = units.iter().zip(&starts).map(|(unit, &start)| (start, start + utf16(&unit.text))).collect::<Vec<_>>();
    let mut joins = Vec::new();
    if join_bodies {
        for citation in citations.iter().filter(|citation| citation.form != Form::Unknown) {
            let spans = [&citation.full_span, &citation.span].into_iter().chain(&citation.style)
                .chain(citation.pinpoints.iter().map(|pinpoint| &pinpoint.span)).collect::<Vec<_>>();
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

/// A brief's citations as its book of authorities reads them, before the statutes it names are looked up: its text and
/// notes; the citations extracted from them (McGill's and COAL's forms, a reference read before its full citation),
/// less the brief's own court file number and any web address with no title before it; its note parts and what each
/// cites; its units as joined (`join_bodies`, a PDF's), where each starts and its citations in reading order; and each
/// cited case's aliases.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BriefReading {
    pub text: String,
    pub notes: Vec<NoteRange>,
    pub citations: Vec<Citation>,
    pub source_parts: Vec<SourcePart>,
    pub references: Vec<ReferencePart>,
    pub units: Vec<AuthorityTextUnit>,
    pub starts: Vec<usize>,
    pub order: Vec<usize>,
    pub alias_groups: Vec<SourceAliasGroup>,
}

/// `file_number`: the brief's own (its cover's); `aliases`: each case key's aliases, as the A2AJ store knows them.
pub fn brief_reading(units: &[AuthorityTextUnit], split_tier: SplitTier, join_bodies: bool, file_number: Option<&str>,
    aliases: impl FnOnce(Vec<String>) -> Result<Vec<Vec<String>>, String>) -> Result<BriefReading, String> {
    let BriefText { text, notes } = brief_text(units);
    let options = Options { resolve: false, notes: Some(notes.clone()), styles: Some(vec![CitationStyle::Mcgill, CitationStyle::Coal]),
        early_references: true, split_tier: Some(split_tier), ..Options::default() };
    let extracted = api::extract(&api::ExtractRequest { text: text.clone(), markup_text: None, options, offset_unit: OffsetUnit::Utf16 })
        .map_err(|error| error.to_string())?;
    let digits = |value: &str| value.chars().filter(char::is_ascii_digit).collect::<String>();
    let own = file_number.map(digits).filter(|own| !own.is_empty());
    let citations = extracted.citations.into_iter().filter(|citation| {
        let titled = citation.style.as_ref().is_some_and(|style| !style.text.trim().is_empty());
        // The brief's own court file number is never one of its authorities, though an order made under it that the
        // brief names ("Re Ash Ltd, Order of Madam Justice Fen dated …, Court File No. 1601-06131") is.
        let own_file = own.is_some() && citation.format == Some(Format::Docket)
            && !(titled && ["Order of", "Endorsement of"].iter().any(|words| citation.full_span.text.contains(words)))
            && citation.fields.docket.as_deref().map(digits) == own;
        // A web address written with no title of a work before it points the reader to a site; it names no authority.
        !own_file && !(citation.form == Form::Full && citation.authority == Authority::Webpage && !titled)
    }).collect::<Vec<_>>();
    let BriefCitationOrder { units, starts, order } = brief_citation_order(units, &citations, join_bodies);
    let cases = citations.iter().filter(|citation| citation.form == Form::Full && matches!(citation.authority, Authority::Case | Authority::Unknown))
        .filter_map(|citation| Some((citation.index, citation.key.clone().filter(|key| !key.is_empty())?))).collect::<Vec<_>>();
    let alias_groups = aliases(cases.iter().map(|(_, key)| key.clone()).collect())?.into_iter().zip(&cases)
        .map(|(keys, &(index, _))| SourceAliasGroup { index, keys }).collect();
    // The note parts that cite: only these can be an authority (a prose note is never one).
    let references = references(&extracted.source_parts, Some(&citations), None, None, utf16);
    Ok(BriefReading { text, notes, citations, source_parts: extracted.source_parts, references, units, starts, order, alias_groups })
}

/// What the brief's authorities are resolved from: its reading, its source parts with the statute pages found for them.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BriefSources {
    pub text: String,
    pub notes: Vec<NoteRange>,
    pub citations: Vec<Citation>,
    pub source_parts: Vec<SourcePart>,
    pub order: Vec<usize>,
    pub alias_groups: Vec<SourceAliasGroup>,
    pub split_tier: SplitTier,
}

/// Every reference resolved (every part reaches the resolver, prose included, so an ibid after a prose note continues
/// from it) at the setting's tier, a reference read by the words before its note's marker; the citations grouped by
/// authority; and each authority's reading: its representative citation, its name, its citation as written and the
/// authority it is subsequent history of.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct BriefAuthorities {
    #[serde(flatten)]
    pub resolved: ResolveResponse,
    pub readings: Vec<AuthorityReading>,
}

pub fn brief_authorities(sources: BriefSources) -> Result<BriefAuthorities, String> {
    let resolved = api::resolve(&api::ResolveRequest { citations: sources.citations, source_parts: sources.source_parts,
        supra_hint_mode: Some(SupraMode::Aggressive), supra_linking_mode: None, split_tier: Some(sources.split_tier), linking: None,
        extents: Vec::new(), reading_order: Some(sources.order), notes: Some(sources.notes), alias_groups: sources.alias_groups,
        text: Some(sources.text), offset_unit: OffsetUnit::Utf16 }).map_err(|error| error.to_string())?;
    let readings = authorities(&AuthoritiesRequest { citations: resolved.citations.clone(), authorities: resolved.authorities.clone() });
    Ok(BriefAuthorities { resolved, readings })
}
