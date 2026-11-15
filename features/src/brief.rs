//! A brief read as its text units (its body paragraphs and its notes): the text the citation engine reads them as,
//! where each note stands in it, which body units a citation runs across, its citations in reading order (a note's
//! after the body up to its call), and the authorities they cite. Offsets count UTF-16 units, as the host's text does.

use serde::{Deserialize, Serialize};
use legal_structure::citations::aliases::SourceAliasGroup;
use indexmap::IndexMap;
use legal_structure::citations::api::{self, QuoteCitationPart};
use legal_structure::citations::authorities::{authorities, AuthoritiesRequest};
use legal_structure::citations::source::{references, ReferencePart, SourcePart};
use legal_structure::citations::url::{canlii_page, CanliiPage};
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
/// cited case's aliases; and the splitting tier its references are resolved at.
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
    /// The note parts (by their place in `references`) whose one statute's CanLII page is to be checked against A2AJ.
    pub statutes: Vec<StatuteCandidate>,
    pub split_tier: SplitTier,
}

/// A note part citing one federal, Ontario or B.C. statute by a name the part writes, whose CanLII page its citation
/// gives (`url`): the page is kept once A2AJ holds that statute (`statute_urls`).
#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct StatuteCandidate {
    /// The part's place in `references` and its text, and the citation as written.
    pub part: usize,
    pub text: String,
    pub citation: String,
    pub url: String,
    pub language: String,
    /// A2AJ's dataset of the jurisdiction's statutes ("LEGISLATION-FED").
    pub dataset: String,
    pub jurisdiction: String,
    pub name: String,
    /// The statute's series, year and chapter as one key ("rso1990cc43").
    pub core: String,
}

/// A statute A2AJ found for a candidate: its dataset, title and citation.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct StatuteHit {
    pub collection: Option<String>,
    pub title: Option<String>,
    pub citation: Option<String>,
}

#[cfg(all(feature = "canada", feature = "canlii"))]
const STATUTE_DATASETS: [(&str, &str); 3] = [("ca", "LEGISLATION-FED"), ("on", "LEGISLATION-ON"), ("bc", "LEGISLATION-BC")];

#[cfg(all(feature = "canada", feature = "canlii"))]
/// A name as a statute's is compared: compatibility-normalized, lower case, letters, digits and single spaces.
fn source_name(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let kept = value.nfkc().collect::<String>().to_lowercase().chars()
        .filter(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || *character == ' ').collect::<String>();
    kept.split(' ').filter(|word| !word.is_empty()).collect::<Vec<_>>().join(" ")
}

/// A statute's name as statute names are compared: two names alike are one name (`source_name`).
#[cfg(all(feature = "canada", feature = "canlii"))]
pub fn statute_name_key(value: &str) -> String {
    source_name(value)
}

#[cfg(all(feature = "canada", feature = "canlii"))]
fn slug_key(value: &str) -> String {
    value.to_lowercase().chars().filter(|character| character.is_ascii_lowercase() || character.is_ascii_digit()).collect()
}

#[cfg(all(feature = "canada", feature = "canlii"))]
/// A statute citation's series, year and chapter as one key.
fn statute_core(citation: &Citation) -> Option<String> {
    let fields = &citation.fields;
    let [series, year, chapter] = [&fields.series, &fields.year, &fields.chapter].map(|value| value.as_deref().filter(|value| !value.is_empty()));
    Some(slug_key(&format!("{}-{}-c-{}", series?, year?, chapter?)))
}

/// A federal, Ontario or B.C. statute's CanLII page, as its address reads: its language, jurisdiction and id.
#[cfg(all(feature = "canada", feature = "canlii"))]
fn canlii_statute(url: Option<&str>) -> Option<(legal_structure::citations::url::CanliiLawPage, &'static str)> {
    let page = legal_structure::citations::url::canlii_law_page(url?)?;
    let dataset = STATUTE_DATASETS.iter().find(|(jurisdiction, _)| *jurisdiction == page.jurisdiction)?.1;
    ["stat", "astat"].contains(&page.kind.as_str()).then_some((page, dataset))
}

/// Each note part that cites one statute in full, by a name the part writes, at a CanLII page of a federal, Ontario or
/// B.C. statute whose id its citation's series, year and chapter begin (or begin with).
#[cfg(all(feature = "canada", feature = "canlii"))]
fn statute_candidates(references: &[ReferencePart], citations: &[Citation]) -> Vec<StatuteCandidate> {
    references.iter().enumerate().filter_map(|(at, reference)| {
        let part = &reference.part;
        let full = citations.iter().filter(|citation| citation.form == Form::Full && citation.authority == Authority::Statute
            && part.start <= citation.span.start && citation.span.end <= part.end).collect::<Vec<_>>();
        let [citation] = full.as_slice() else { return None };
        let core = statute_core(citation)?;
        let name = citation.short_name.as_deref().map(str::trim).filter(|name| !name.is_empty())?;
        if !source_name(&part.text).contains(&source_name(name)) { return None; }
        let url = api::url(&api::UrlRequest { citation: Some((*citation).clone()), anchor: true, ..Default::default() }).ok()?
            .urls.into_iter().next().and_then(|url| url.url)?;
        let (page, dataset) = canlii_statute(Some(&url))?;
        let id = slug_key(&page.id);
        (id.starts_with(&core) || core.starts_with(&id)).then(|| StatuteCandidate { part: at, text: part.text.clone(),
            citation: citation.span.text.clone(),
            url: url.clone(), language: page.language.clone(), dataset: dataset.to_owned(), jurisdiction: page.jurisdiction.clone(),
            name: name.to_owned(), core })
    }).collect()
}

/// The CanLII pages, at the candidate's anchor, of the statutes A2AJ found (`hits`) that are the candidate's: of its
/// dataset, titled as its part writes, of its series, year and chapter, and at a page of its language and jurisdiction.
/// Each once, in the order found.
#[cfg(all(feature = "canada", feature = "canlii"))]
pub fn statute_urls(candidate: &StatuteCandidate, hits: &[StatuteHit]) -> Vec<String> {
    let written = source_name(&candidate.text);
    // The candidate's address is a serialized URL: its anchor is what follows its "#".
    let anchor = candidate.url.split_once('#').map(|(_, anchor)| anchor).filter(|anchor| !anchor.is_empty())
        .map(|anchor| format!("#{anchor}")).unwrap_or_default();
    let mut urls = Vec::<String>::new();
    for hit in hits {
        let Some(title) = hit.title.as_deref().filter(|title| !title.is_empty()) else { continue };
        if hit.collection.as_deref().map(str::to_uppercase).as_deref() != Some(candidate.dataset.as_str())
            || !written.contains(&source_name(title)) { continue; }
        let cited = hit.citation.as_deref().unwrap_or("");
        let Ok(parsed) = api::extract(&api::ExtractRequest { text: cited.to_owned(), markup_text: None,
            options: Options { resolve: false, ..Options::default() }, offset_unit: OffsetUnit::default() }) else { continue };
        let statutes = parsed.citations.iter().filter(|citation| citation.form == Form::Full && citation.authority == Authority::Statute)
            .collect::<Vec<_>>();
        if statutes.len() != 1 || statute_core(statutes[0]).as_deref() != Some(candidate.core.as_str()) { continue; }
        let Some(base) = canlii_page(statutes[0]).and_then(|page| page.page(&candidate.language, true).map(str::to_owned))
            else { continue };
        let Some((route, _)) = canlii_statute(Some(&base)) else { continue };
        if route.language != candidate.language || route.jurisdiction != candidate.jurisdiction { continue; }
        let url = format!("{}{anchor}", base.split('#').next().unwrap_or(""));
        if !urls.contains(&url) { urls.push(url); }
    }
    urls
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
    #[cfg(all(feature = "canada", feature = "canlii"))]
    let statutes = statute_candidates(&references, &citations);
    #[cfg(not(all(feature = "canada", feature = "canlii")))]
    let statutes = Vec::new();
    Ok(BriefReading { text, notes, citations, source_parts: extracted.source_parts, references, units, starts,
        order, alias_groups, statutes, split_tier })
}

/// A span of a unit's text, in its UTF-16 offsets, and its words.
type TextSpan = legal_structure::citations::Span;

/// The brief as its book of authorities' draft reviews it: its units (as joined) with their citations' occurrences and a
/// note's parts, each occurrence, and each authority in the order first cited.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BriefReview {
    pub units: Vec<BriefUnit>,
    #[cfg_attr(feature = "binding-types", ts(type = "Record<string, BriefOccurrence>"))]
    pub occurrences: IndexMap<String, BriefOccurrence>,
    #[cfg_attr(feature = "binding-types", ts(type = "Record<string, BriefAuthority>"))]
    pub authorities: IndexMap<String, BriefAuthority>,
    pub authority_order: Vec<String>,
}

/// A unit of the draft: its citations' occurrences, and a note's parts as the engine split it (offsets in the note), each
/// with the first authority it refers to and the page its first full citation of a report starts at.
#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "ReviewUnit"))]
#[serde(rename_all = "camelCase")]
pub struct BriefUnit {
    #[serde(flatten)]
    pub unit: AuthorityTextUnit,
    pub occurrence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub parts: Option<Vec<QuoteCitationPart>>,
}

/// A citation where its unit writes it: its whole extent, its authority's citation (the style its core is written with),
/// its core, its pinpoints and the words that introduce them ("at para 105"), what it cites, and the reference it is.
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "ReviewOccurrence"))]
#[serde(rename_all = "camelCase", default)]
pub struct BriefOccurrence {
    pub id: String,
    pub unit_id: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub authority_span: TextSpan,
    pub core_span: TextSpan,
    pub pinpoint_span: Option<TextSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub pinpoint_phrase: Option<TextSpan>,
    /// The authority's kind ("case", "legislation", "commentary", "other"), or "reference".
    pub kind: String,
    pub citation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub key: Option<String>,
    pub authority_id: Option<String>,
    pub reference: Option<OccurrenceReference>,
    /// "short", "ibid" or "supra".
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub reference_kind: Option<String>,
    pub pinpoints: Vec<OccurrencePinpoint>,
    pub evidence_ids: Vec<String>,
    pub source_text_sha256: String,
    pub local_ordinal: usize,
    pub reviewed: bool,
    /// A link an editor gave the citation since.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub link: Option<String>,
}

/// The authority a reference stands for, and where the engine resolved it: the occurrence its chain ends at (its origin)
/// and the one it names directly.
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct OccurrenceReference {
    pub kind: String,
    pub target_authority_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub origin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub named_id: Option<String>,
}

/// A pinpoint as the engine parsed it: a range as its first and last locators ("62-64"), never inferred from the gaps
/// between them; and where its unit writes it.
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(default)]
pub struct OccurrencePinpoint {
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub first: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub last: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub end: Option<usize>,
}

/// An authority as the draft holds it: what the brief's citations say of it (its kind, its citation and name as written,
/// a decision's other names, the CanLII pages and web address its citations give, the authority it is subsequent history
/// of), and what the draft decides of it since (its source and the source's identity, the case its citation names).
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct BriefAuthority {
    pub id: String,
    pub key: String,
    pub kind: String,
    /// "database" or "docket": a decision cited only by a database identifier or a court file number.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub citation_format: Option<String>,
    pub citation: String,
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<String>>", optional))]
    pub short_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub mentioned_as: Option<String>,
    pub display_name: Option<String>,
    pub excluded: bool,
    pub evidence_ids: Vec<String>,
    pub locators: Vec<Locator>,
    pub source_identity: Option<SourceIdentity>,
    pub source: SourceDecision,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<bool>", optional))]
    pub scan_only: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<CanliiPage>>", optional))]
    pub canlii: Vec<CanliiPage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub history_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub cited_case: Option<CitedCase>,
}

/// A passage of a source: its locator's kind and label.
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct Locator {
    pub kind: String,
    pub label: String,
}

/// Where an authority's source stands: "unresolved", "resolved", "attached" (its PDFs) or "pending-canlii" (its page).
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct SourceDecision {
    pub kind: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<AttachedSource>>", optional))]
    pub sources: Vec<AttachedSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub page_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct AttachedSource {
    pub source_url: Option<String>,
}

/// The source a provider holds for an authority: the provider, its id there, its page, and its CanLII page as the engine
/// read it off the source's citations.
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct SourceIdentity {
    pub provider: String,
    pub stable_source_id: String,
    pub external_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional = nullable))]
    pub canlii_url: Option<String>,
}

/// The case a source holds at a decision's citation, where it is not the one the brief names.
#[derive(Serialize, Deserialize, Clone, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(default)]
pub struct CitedCase {
    pub id: String,
    pub citation: String,
    pub name: String,
}

/// The first of `others` (citations as written, style and all) that writes the docket `span` after a space and is a
/// letter or two from `written`: the same order under a name its recognizer misread ("Re Pacifio …" for "Re Pacific
/// …"), by its place among them. Two texts of at least 12 UTF-16 units each, at most two apart in length, within two
/// edits of each other (letters added, dropped or changed, counted in UTF-16 units).
fn misread_docket<'a>(written: &str, span: &str, others: impl Iterator<Item = &'a String>) -> Option<usize> {
    let mut docket = String::from(" ");
    for word in span.split(legal_structure::javascript_whitespace).filter(|word| !word.is_empty()) {
        if docket.len() > 1 { docket.push(' '); }
        docket.push_str(word);
    }
    let written = written.encode_utf16().collect::<Vec<_>>();
    others.into_iter().position(|other| {
        let other_units = other.encode_utf16().collect::<Vec<_>>();
        other.ends_with(&docket) && other_units.len().min(written.len()) >= 12 && other_units.len().abs_diff(written.len()) <= 2
            && strsim::generic_levenshtein(&other_units, &written) <= 2
    })
}

/// A unit's occurrences (in the order the unit holds them) with their overlaps trimmed, their words read again from the
/// unit (`text`, its UTF-16 units): the parser can read two citations' full extents over one another though their cores
/// are apart. One whose core starts inside the one before it (its core, or where its pinpoints as written end) is
/// dropped; else the shared context is cut where the earlier one's core and pinpoints end, or where the later one starts,
/// and the later one starts no sooner than the earlier ends.
fn trimmed(occurrences: Vec<BriefOccurrence>, text: &[u16]) -> Vec<BriefOccurrence> {
    let mut kept = Vec::<BriefOccurrence>::with_capacity(occurrences.len());
    // Where the last kept occurrence's core and pinpoints end.
    let mut core_end = 0;
    for mut current in occurrences {
        if let Some(previous) = kept.last_mut().filter(|previous| current.start < previous.end) {
            if core_end > current.core_span.start { continue; }
            let boundary = core_end.max(current.start);
            if boundary < previous.end {
                previous.end = boundary;
                previous.authority_span.end = previous.authority_span.end.min(boundary);
            }
            if current.start < previous.end {
                current.start = previous.end;
                current.authority_span.start = current.authority_span.start.max(current.start);
            }
        }
        core_end = current.core_span.end.max(current.pinpoint_phrase.as_ref().or(current.pinpoint_span.as_ref())
            .map_or(0, |span| span.end));
        kept.push(current);
    }
    for occurrence in &mut kept {
        occurrence.text = utf16_slice(text, occurrence.start, occurrence.end);
        occurrence.authority_span.text = utf16_slice(text, occurrence.authority_span.start, occurrence.authority_span.end);
    }
    kept
}

/// The UTF-16 units `start..end` of a text, as JavaScript's slice takes them.
fn utf16_slice(text: &[u16], start: usize, end: usize) -> String {
    let end = end.min(text.len());
    String::from_utf16_lossy(&text[start.min(end)..end])
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Words joined by single spaces, as a line ("RSC ⏎⏎ 1985" is "RSC 1985").
fn one_line(value: &str) -> String {
    value.split(legal_structure::javascript_whitespace).filter(|word| !word.is_empty()).collect::<Vec<_>>().join(" ")
}

/// The kind of authority the draft files a citation's under.
fn kind_of(authority: Authority) -> &'static str {
    match authority {
        Authority::Case => "case",
        authority if authority.is_legislation() => "legislation",
        Authority::Journal | Authority::Book | Authority::BookChapter | Authority::Webpage => "commentary",
        _ => "other",
    }
}

/// A web address a reader can open: http or https, no credentials, at most 8192 UTF-16 units.
fn observed(value: &str) -> bool {
    utf16(value) <= 8192 && url::Url::parse(value)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none())
}

/// A link written without its scheme ("online: <example.org/report>") is the web address it names.
fn with_scheme(link: Option<&str>) -> Option<String> {
    static BARE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let bare = BARE.get_or_init(|| regex::Regex::new(r"(?i)^[a-z0-9-]+(?:\.[a-z0-9-]+)+(?:[/?#]|$)").expect("bare link"));
    link.map(|link| if bare.is_match(link) { format!("https://{link}") } else { link.to_owned() })
}

/// A value's name as it is serialized ("paragraph").
fn serde_name(value: impl Serialize) -> String {
    serde_json::to_value(value).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default()
}

/// What a unit holds, in the order the text writes it: a citation, or the part of a note citing a source no citation
/// names (its reference, its authority's key).
enum Item<'a> {
    Citation(&'a Citation),
    Source(&'a legal_structure::citations::source::Reference, String),
}

/// A brief read, reviewed: each note part citing one statute takes the CanLII page A2AJ confirmed for it (`pages`, by
/// statute candidate, where exactly one differs from the one its citation gives); every reference is resolved at the
/// reading's tier (every part reaches the resolver, prose included, so an ibid after a prose note continues from it, a
/// reference read by the words before its note's marker); the citations are grouped by authority, each authority read
/// (its representative citation, name, citation as written, and the authority it is subsequent history of); and each
/// citation, and each note part citing a source no citation names, is placed in its unit as an occurrence.
pub fn brief_review(reading: BriefReading, pages: &[Vec<String>]) -> Result<BriefReview, String> {
    let BriefReading { text, notes, citations, source_parts: read_parts, references, units, starts, order, alias_groups,
        statutes, split_tier } = reading;
    let part_index = |part: &SourcePart| read_parts.iter().rposition(|other| other.start == part.start && other.end == part.end);
    let mut source_parts = read_parts.clone();
    for (candidate, pages) in statutes.iter().zip(pages) {
        if let ([page], Some(at)) = (pages.as_slice(), part_index(&references[candidate.part].part)) {
            if *page != candidate.url { source_parts[at].resolved_url = Some(page.clone()); }
        }
    }
    let resolved = api::resolve(&api::ResolveRequest { citations: citations.clone(), source_parts: source_parts.clone(),
        supra_hint_mode: Some(SupraMode::Aggressive), supra_linking_mode: None, split_tier: Some(split_tier), linking: None,
        extents: Vec::new(), reading_order: Some(order), notes: Some(notes), alias_groups,
        text: Some(text.clone()), offset_unit: OffsetUnit::Utf16 }).map_err(|error| error.to_string())?;
    let readings = authorities(&AuthoritiesRequest { citations: resolved.citations.clone(), authorities: resolved.authorities.clone() });
    let by_index = resolved.citations.iter().map(|citation| (citation.index, citation)).collect::<HashMap<_, _>>();
    let by_resolution = resolved.resolutions.iter().map(|resolution| (resolution.index, resolution)).collect::<HashMap<_, _>>();
    let document_hash = sha256_hex(&text);
    // The note parts a reference's resolution ends at that no citation of theirs names: each is an authority of its own.
    let origins = resolved.resolutions.iter().filter_map(|resolution| resolution.source_part).collect::<HashSet<_>>();
    let origin_parts = references.iter().filter_map(|source| {
        let index = part_index(&source.part).filter(|index| origins.contains(index))?;
        source.references.iter().all(|reference| reference.form != "citation").then_some(())?;
        Some((index, source.references.first()?))
    }).collect::<Vec<_>>();
    // The link each citation writes ("online: <…>", a URL in its text), by citation.
    let link_of = references.iter().flat_map(|source| &source.references).filter_map(|reference|
        Some((reference.citation?, reference.link.as_deref().filter(|link| !link.is_empty())?))).collect::<HashMap<_, _>>();
    let mut authorities_found = IndexMap::<String, BriefAuthority>::new();
    let mut authority_order = Vec::new();
    let mut group_keys = vec![None::<String>; resolved.authorities.len()];
    let mut authority_of = HashMap::<usize, String>::new();
    let mut source_groups = HashMap::<usize, String>::new();
    // Citations with no key, as written (style and all): one written alike names the same authority.
    let mut unkeyed = IndexMap::<String, String>::new();
    for (position, group) in resolved.authorities.iter().enumerate() {
        let reading = readings.get(position);
        let full = group.iter().filter_map(|index| by_index.get(index).copied()).filter(|citation| citation.form == Form::Full)
            .collect::<Vec<_>>();
        let source_url = full.first().filter(|_| full.len() == 1).and_then(|citation| source_parts.iter()
            .find(|part| part.resolved_url.as_deref().is_some_and(|url| !url.is_empty())
                && part.start <= citation.span.start && citation.span.end <= part.end))
            .and_then(|part| part.resolved_url.clone());
        let reference_url = group.iter().filter_map(|index| by_resolution.get(index)?.url.as_deref()).find(|url| observed(url));
        let url = source_url
            .or_else(|| full.iter().filter_map(|citation| with_scheme(link_of.get(&citation.index).copied()
                .or(citation.fields.url.as_deref()))).find(|url| observed(url)))
            .or_else(|| reference_url.map(|url| (if full.is_empty() { url } else { url.split('#').next().unwrap_or("") }).to_owned()));
        let origin = group.iter().find_map(|index| by_resolution.get(index)?.source_part);
        let source = if full.is_empty() { origin_parts.iter().find(|(index, _)| Some(*index) == origin) } else { None };
        let representative = reading.and_then(|reading| reading.representative).and_then(|index| by_index.get(&index).copied())
            .or_else(|| source.and_then(|_| group.iter().find_map(|index| by_index.get(index).copied())));
        let Some(representative) = representative else { continue };
        // An order cited by its judge, date and number ("Order of Mr. Justice Butler dated August 13, 2015, No.
        // S-154746") is that order whatever matter is written before it.
        let docket = representative.format == Some(Format::Docket);
        let self_named = docket && representative.span.text.contains("Order of");
        let written = (!full.is_empty()).then(|| one_line(&format!("{} {}",
            if self_named { "" } else { representative.style.as_ref().map_or("", |style| style.text.as_str()) },
            representative.span.text))).filter(|written| !written.is_empty());
        // A recognizer may misread a letter of the name ("Re Pacifio …" for "Re Pacific …"): an order cited by the same
        // file number under a name a letter or two apart is the same order.
        let misread = written.as_deref().filter(|_| docket)
            .and_then(|written| misread_docket(written, &representative.span.text, unkeyed.keys()))
            .and_then(|at| unkeyed.get_index(at)).map(|(_, key)| key.clone());
        let key = representative.key.clone().filter(|key| !full.is_empty() && !key.is_empty())
            .or_else(|| written.as_ref().and_then(|written| unkeyed.get(written).cloned().or(misread)))
            .unwrap_or_else(|| match source {
                Some((index, _)) => format!("scan:{document_hash}:source:{index}"),
                None => format!("scan:{document_hash}:{}", representative.index),
            });
        if let Some(written) = written { unkeyed.insert(written, key.clone()); }
        for index in group { authority_of.insert(*index, key.clone()); }
        group_keys[position] = Some(key.clone());
        if authorities_found.contains_key(&key) { continue; }
        let source_url = with_scheme(source.and_then(|(_, reference)| reference.link.as_deref()).or(url.as_deref()))
            .filter(|link| observed(link));
        if let Some((index, _)) = source { source_groups.insert(*index, key.clone()); }
        let observed_text = match source {
            Some((_, reference)) => reference.text.clone(),
            None if !full.is_empty() => reading.and_then(|reading| reading.citation.clone()).unwrap_or_default(),
            None => representative.full_span.text.clone(),
        };
        // A decision's other names in the brief: its short forms, and the case the sentence a note hangs from names. They
        // tell whether the case a source holds at its citation is the one meant.
        let case = representative.authority == Authority::Case;
        let mut short_names = Vec::new();
        for name in full.iter().filter(|_| case).filter_map(|citation| citation.explicit_short_name.as_deref()) {
            let name = one_line(name);
            if !name.is_empty() && !short_names.contains(&name) { short_names.push(name); }
        }
        let mentioned_as = full.iter().filter(|_| case).filter_map(|citation| citation.fields.anchor_mention.as_ref())
            .map(|mention| one_line(&mention.text)).find(|mention| !mention.is_empty());
        authorities_found.insert(key.clone(), BriefAuthority { id: key.clone(), key: key.clone(),
            kind: kind_of(source.map_or(representative.authority, |(_, reference)| reference.authority)).to_owned(),
            citation_format: (!full.is_empty() && full.iter().all(|citation| citation.authority == Authority::Case
                && matches!(citation.format, Some(Format::Database | Format::Docket))))
                .then(|| if docket { "docket" } else { "database" }.to_owned()),
            // A citation or name a PDF's line or page break runs through is one line ("RSC ⏎⏎ 1985").
            citation: one_line(&observed_text),
            name: if source.is_some() { None } else { reading.and_then(|reading| reading.name.clone()) },
            short_names, mentioned_as,
            source: SourceDecision { kind: "unresolved".to_owned(), ..Default::default() }, scan_only: true,
            canlii: resolved.authorities[position].iter().filter_map(|index| by_index.get(index))
                .filter_map(|citation| canlii_page(citation)).collect(),
            source_url, ..Default::default() });
        authority_order.push(key);
    }
    // A decision the engine reads as another's subsequent history ("…, 2010 ABQB 242, aff'd 2010 ABCA 191") records which
    // authority it follows, and is known by its case's name unless the brief gives it one of its own.
    for (position, reading) in readings.iter().enumerate() {
        let parent = reading.history_of.and_then(|at| group_keys.get(at).cloned().flatten());
        let (Some(child), Some(parent)) = (group_keys[position].clone(), parent) else { continue };
        if child == parent || authorities_found[&child].history_of.is_some() { continue; }
        let parent_name = authorities_found[&parent].name.clone();
        let authority = &mut authorities_found[&child];
        authority.history_of = Some(parent);
        if authority.name.is_none() { authority.name = reading.name.clone().or(parent_name); }
    }
    let mut items = resolved.citations.iter().map(|citation| (citation.span.start, Item::Citation(citation)))
        .chain(origin_parts.iter().filter_map(|(index, reference)|
            Some((reference.start, Item::Source(*reference, source_groups.get(index)?.clone())))))
        .collect::<Vec<_>>();
    items.sort_by_key(|(start, _)| *start);
    // A note's parts, as the engine split it, in the note's offsets.
    let parts_of = |start: usize, end: usize| read_parts.iter().filter(|part| start <= part.start && part.end <= end).map(|part| {
        let first_page = citations.iter().find(|citation| citation.form == Form::Full
            && citation.fields.page.as_deref().is_some_and(|page| !page.is_empty())
            && part.start <= citation.span.start && citation.span.end <= part.end).and_then(|citation| citation.fields.page.clone());
        let reference = references.iter().find(|item| item.part.start == part.start && item.part.end == part.end)
            .and_then(|item| item.references.first()).map(|reference| reference.text.clone()).unwrap_or_default();
        let mut source = part.clone();
        source.start -= start;
        source.end -= start;
        for anchor in &mut source.anchor_spans { *anchor = (anchor.0.saturating_sub(start), anchor.1.saturating_sub(start)); }
        QuoteCitationPart { source, reference, first_page }
    }).collect::<Vec<_>>();
    let mut cursor = 0;
    let mut occurrences = IndexMap::<String, BriefOccurrence>::new();
    let (mut occurrence_of, mut start_of) = (HashMap::<usize, String>::new(), HashMap::<String, usize>::new());
    let mut placed_units = Vec::with_capacity(units.len());
    for (unit, &start) in units.into_iter().zip(&starts) {
        let wide = unit.text.encode_utf16().collect::<Vec<_>>();
        let end = start + wide.len();
        let span = |(from, to): (usize, usize)| TextSpan { start: from, end: to, text: utf16_slice(&wide, from, to) };
        // A span of the text, clamped to the unit, in its offsets.
        let local = |span: &TextSpan| (span.start.max(start) - start, span.end.min(end).saturating_sub(start));
        let source_text_sha256 = sha256_hex(&unit.text);
        let mut placed = Vec::new();
        while cursor < items.len() && items[cursor].0 < start { cursor += 1; }
        while cursor < items.len() && items[cursor].0 < end {
            let (_, item) = &items[cursor];
            cursor += 1;
            let (local_ordinal, id) = (placed.len(), format!("{}:{}", unit.key, placed.len()));
            let occurrence = match item {
                Item::Source(reference, authority_id) => {
                    let full = (reference.start - start, reference.end - start);
                    let authority = &authorities_found[authority_id];
                    BriefOccurrence { start: full.0, end: full.1, text: span(full).text, authority_span: span(full), core_span: span(full),
                        kind: authority.kind.clone(), citation: authority.citation.clone(), authority_id: Some(authority_id.clone()),
                        ..Default::default() }
                }
                Item::Citation(citation) if citation.form != Form::Unknown => {
                    let authority_id = authority_of.get(&citation.index).cloned();
                    let form = matches!(citation.form, Form::Short | Form::Ibid | Form::Supra).then(|| serde_name(citation.form));
                    let pinpoints = citation.pinpoints.iter().filter(|pinpoint| start <= pinpoint.span.start && pinpoint.span.end <= end)
                        .collect::<Vec<_>>();
                    let core = local(citation.fields.inline_reference.as_ref().map_or(&citation.span, |marker| &marker.span));
                    let full = local(&citation.full_span);
                    let styled = match &citation.style {
                        Some(style) if style.end > start && style.start <= citation.span.start => full.0.max(local(style).0),
                        _ => core.0,
                    };
                    let (pinpoint, phrase) = legal_structure::pinpoint_extent(pinpoints.iter().map(|pinpoint| local(&pinpoint.span)),
                        citation.fields.pin_cite.as_ref().map(local));
                    occurrence_of.insert(citation.index, id.clone());
                    BriefOccurrence { start: full.0, end: full.1, text: span(full).text, authority_span: span((styled, core.1)),
                        core_span: span(core), pinpoint_span: pinpoint.map(span), pinpoint_phrase: phrase.map(span),
                        kind: if citation.form != Form::Full { "reference".to_owned() } else { kind_of(citation.authority).to_owned() },
                        citation: citation.span.text.clone(), key: citation.key.clone().filter(|key| !key.is_empty()),
                        reference: authority_id.clone().zip(form.clone())
                            .map(|(target_authority_id, kind)| OccurrenceReference { kind, target_authority_id, ..Default::default() }),
                        reviewed: citation.form != Form::Full && authority_id.is_some(), authority_id, reference_kind: form,
                        // A range as parsed, never inferred from the gaps between its locators; a single pinpoint names its
                        // locator as the engine reads it ("110ff" locates para 110).
                        pinpoints: pinpoints.iter().map(|pinpoint| {
                            let (from, to) = local(&pinpoint.span);
                            let last = pinpoint.last.clone().filter(|last| !last.is_empty());
                            OccurrencePinpoint { kind: serde_name(pinpoint.kind),
                                text: last.as_ref().map_or_else(|| pinpoint.first.clone(), |last| format!("{}-{last}", pinpoint.first)),
                                first: Some(pinpoint.first.clone()).filter(|first| !first.is_empty()), last,
                                start: Some(from), end: Some(to) }
                        }).collect(),
                        ..Default::default() }
                }
                Item::Citation(_) => continue,
            };
            start_of.insert(id.clone(), start + occurrence.start);
            placed.push(BriefOccurrence { id, unit_id: unit.key.clone(), source_text_sha256: source_text_sha256.clone(), local_ordinal,
                ..occurrence });
        }
        let parts = (unit.kind == "footnote" && unit.footnote_id.is_some()).then(|| parts_of(start, end));
        placed_units.push((BriefUnit { unit, occurrence_ids: placed.iter().map(|occurrence| occurrence.id.clone()).collect(), parts }, wide));
        occurrences.extend(placed.into_iter().map(|occurrence| (occurrence.id.clone(), occurrence)));
    }
    // Where the engine resolved each reference, as its citations of the authority: the one its chain ends at (its origin)
    // and the one it names directly, each the first of the authority's in the part the engine gives.
    let mut sorted = occurrences.keys().cloned().collect::<Vec<_>>();
    sorted.sort_by_key(|id| start_of[id]);
    let mut cites = HashMap::<Option<String>, Vec<String>>::new();
    for id in sorted { cites.entry(occurrences[&id].authority_id.clone()).or_default().push(id); }
    for resolution in &resolved.resolutions {
        let Some(id) = occurrence_of.get(&resolution.index) else { continue };
        let Some(target) = occurrences[id].reference.as_ref().map(|reference| Some(reference.target_authority_id.clone())) else { continue };
        let in_part = |index: Option<usize>| {
            let part = source_parts.get(index?)?;
            cites.get(&target)?.iter().find(|other| *other != id && part.start <= start_of[*other] && start_of[*other] < part.end).cloned()
        };
        let antecedent = resolution.antecedent.and_then(|antecedent| occurrence_of.get(&antecedent))
            .filter(|antecedent| occurrences[*antecedent].authority_id == target).cloned();
        let (origin, named) = (in_part(resolution.source_part).or(antecedent), in_part(resolution.target_part));
        let reference = occurrences[id].reference.as_mut().expect("a reference");
        if origin.is_some() { reference.origin_id = origin; }
        if named.is_some() { reference.named_id = named; }
    }
    // The parser can return overlapping full spans even when their citation cores are distinct: both editable mentions
    // are kept, only their shared context trimmed; one whose core starts inside the one before it is dropped.
    let mut kept = IndexMap::with_capacity(occurrences.len());
    let units = placed_units.into_iter().map(|(mut unit, wide)| {
        let placed = trimmed(unit.occurrence_ids.iter().filter_map(|id| occurrences.swap_remove(id)).collect(), &wide);
        unit.occurrence_ids = placed.iter().map(|occurrence| occurrence.id.clone()).collect();
        kept.extend(placed.into_iter().map(|occurrence| (occurrence.id.clone(), occurrence)));
        unit
    }).collect();
    Ok(BriefReview { units, occurrences: kept, authorities: authorities_found, authority_order })
}
