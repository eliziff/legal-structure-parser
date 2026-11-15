//! The Authorities review's findings, read in one pass over a draft and the passages its single-source notes cite: a
//! quotation the cited passage holds otherwise (its words repaired from the source), or one another passage of the
//! source holds (its pinpoint rewritten to it, or the cited range extended to it); and the correction each decision on a
//! finding writes into the brief. Only obvious errors are raised: words found nowhere are none.

use crate::{editorial_quote, footnote_propositions, grounded_prose_errors, marked_quote_spans, quotation_diff, quote_occurrences,
    verbatim_repair, BriefOccurrence, BriefUnit, QuotationDiff, QuoteContext, VisibleEvidenceText};
use legal_structure::citations::{api, authorities, PinpointKind};
use legal_structure::javascript_whitespace;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::LazyLock;

/// A passage of a source, as the review names it by its locator.
#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "DiscrepancyPassage"))]
pub struct Passage {
    pub locator: Locator,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "DiscrepancyLocator"))]
pub struct Locator {
    pub kind: String,
    pub label: String,
}

/// The passage a citation cites, read: its source's version, and the source's other passages of the same kind, where
/// a quotation the cited one lacks may stand.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "DiscrepancySource", optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct CitedSource {
    pub occurrence_id: String,
    pub source_version: Option<String>,
    pub cited: Passage,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<Passage>>", optional))]
    pub alternatives: Vec<Passage>,
}

#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "DiscrepancyPinpoint", optional_fields = nullable))]
pub struct AuthoredPinpoint {
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
}

/// A finding: the quotation, the note and citation it is in, the passage it cites and the one that holds it, what
/// each correction writes, and the decisions it offers.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "DiscrepancyFinding", optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub kind: String,
    pub actions: Vec<String>,
    pub occurrence_id: String,
    pub authority_id: String,
    pub footnote_id: usize,
    pub citation: String,
    pub proposition: String,
    pub authored_quote: String,
    pub authored_pinpoint: AuthoredPinpoint,
    pub cited: Passage,
    pub found: Option<Passage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended: Option<String>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub diff: Option<QuotationDiff>,
}

/// A change to a unit's text: its span (UTF-16), the text it must still hold, and what it becomes.
#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "BriefCorrection"))]
#[serde(rename_all = "camelCase")]
pub struct Correction {
    pub unit_id: String,
    pub start: usize,
    pub end: usize,
    pub expected: String,
    pub replacement: String,
}

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The byte where a UTF-16 offset falls in `text`.
fn byte(text: &str, offset: usize) -> usize {
    let mut units = 0;
    text.char_indices().find(|(_, character)| { let reached = units >= offset; units += character.len_utf16(); reached }).map_or(text.len(), |(at, _)| at)
}

/// A note's marked quotations, as the review weighs them: each once, its blanks one space, of eight characters and two
/// words at least.
fn quotations(text: &str) -> Vec<String> {
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("word"));
    static BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\r\n\x0C\x0B]+").expect("blanks"));
    let mut found = Vec::<String>::new();
    for quote in marked_quote_spans(text) {
        let quote = BLANKS.replace_all(quote.text.trim_matches(javascript_whitespace), " ").into_owned();
        if !found.contains(&quote) { found.push(quote); }
    }
    found.into_iter().filter(|quote| utf16(quote) >= 8 && WORD.find_iter(quote).count() >= 2).collect()
}

/// A number as JavaScript reads one ("12", "012"), or none where it is not one.
fn number(text: &str) -> Option<f64> {
    let text = text.trim_matches(javascript_whitespace);
    if text.is_empty() { return Some(0.0); }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// The pinpoint phrase rewritten to the place a quotation is at, as the citation engine writes the new one ("at paras
/// 121-22" with 123: "at para 123"); `extend` keeps the cited range and reaches the place beside it ("at paras
/// 121-123"), and a place not beside it has no extension.
pub fn pinpoint_correction(occurrence: &BriefOccurrence, label: &str, extend: bool) -> Option<(usize, usize, String, String)> {
    let (pin, phrase) = (occurrence.pinpoints.first()?, occurrence.pinpoint_phrase.as_ref().or(occurrence.pinpoint_span.as_ref())?);
    let first = pin.first.as_deref().filter(|first| !first.is_empty() && occurrence.pinpoints.len() == 1)?;
    let value = authorities::pinpoints_at(&authorities::PinpointsRequest { text: label.to_owned(), start: 0, end: utf16(label), strict: false })
        .into_iter().next().map_or_else(|| label.trim_matches(javascript_whitespace).to_owned(), |pin| pin.first);
    let last = pin.last.as_deref().unwrap_or(first);
    let (start, end, at) = (number(first), number(last), number(&value));
    let whole = [start, end, at].iter().all(|value| value.is_some_and(|value| value.fract() == 0.0));
    let locator = if !extend { api::Locator { first: value.clone(), last: None } }
        else if whole && at == start.map(|start| start - 1.0) { api::Locator { first: value.clone(), last: Some(last.to_owned()) } }
        else if whole && at == end.map(|end| end + 1.0) { api::Locator { first: first.to_owned(), last: Some(value.clone()) } }
        else { return None };
    let kind = serde_json::from_value::<PinpointKind>(serde_json::Value::String(pin.kind.clone())).ok()?;
    let locator_first = locator.first.clone();
    let written = api::format(&api::FormatRequest { pinpoint: Some(api::PinpointRequest { kind, locators: vec![locator] }), ..Default::default() })
        .ok()?.pinpoint.filter(|written| !written.is_empty())?;
    static AT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^at\s+").expect("at"));
    // A span placed by hand may hold only the locators; their introduction stays outside it. Otherwise a phrase the
    // brief writes without "at" keeps it out.
    let replacement = if occurrence.pinpoint_phrase.is_none() {
        written.find(&locator_first).map_or_else(|| written.chars().last().map(String::from).unwrap_or_default(), |at| written[at..].to_owned())
    } else if AT.is_match(&phrase.text) { written } else { AT.replace(&written, "").into_owned() };
    (replacement != phrase.text).then(|| (phrase.start, phrase.end, phrase.text.clone(), replacement))
}

/// The one place in a note's proposition a quotation stands, its blanks read as any blanks.
fn quote_target(units: &[BriefUnit], propositions: &HashMap<u64, crate::propositions::Proposition>, footnote: usize, quote: &str) -> Option<Correction> {
    static BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("{}+", legal_structure::JS_WHITESPACE_CLASS)).expect("blanks"));
    let words = BLANKS.split(quote.trim_matches(javascript_whitespace)).map(regex::escape).collect::<Vec<_>>();
    let pattern = Regex::new(&words.join(&format!("{}+", legal_structure::JS_WHITESPACE_CLASS))).ok()?;
    let proposition = propositions.get(&(footnote as u64))?;
    let mut targets = proposition.parts.iter().flat_map(|part| {
        let unit = &units[part.unit].unit;
        let (from, to) = (byte(&unit.text, part.start), byte(&unit.text, part.end));
        pattern.find_iter(&unit.text[from..to]).map(move |found| Correction { unit_id: unit.key.clone(),
            start: part.start + utf16(&unit.text[from..from + found.start()]), end: part.start + utf16(&unit.text[from..from + found.end()]),
            expected: found.as_str().to_owned(), replacement: String::new() })
    });
    let target = targets.next()?;
    targets.next().is_none().then_some(target)
}

/// A finding's id: dismissing it holds until the note's text, the source's version, the authority, the pinpoint or the
/// quotation changes, never because the brief changed elsewhere (the sha-256 of their canonical JSON, keys sorted).
fn finding_id(occurrence: &BriefOccurrence, source: &CitedSource, kind: &str, quote: &str) -> String {
    #[derive(Serialize)]
    struct Pin<'a> { kind: &'a str, text: &'a str }
    let pins = occurrence.pinpoints.iter().map(|pin| Pin { kind: &pin.kind, text: &pin.text }).collect::<Vec<_>>();
    let version = source.source_version.as_deref().unwrap_or(&source.cited.text);
    let canonical = serde_json::to_string(&("beaver.authorities-discrepancy.v2", &occurrence.unit_id, version, &occurrence.citation,
        &occurrence.authority_id, pins, kind, quote, &source.cited.locator)).expect("finding id");
    Sha256::digest(canonical.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn same_locator(left: &Locator, right: &Locator) -> bool {
    let label = |locator: &Locator| locator.label.chars().filter(|character| !javascript_whitespace(*character))
        .map(|character| if matches!(character, '\u{2013}' | '\u{2014}') { '-' } else { character }).collect::<String>();
    left.kind == right.kind && label(left) == label(right)
}

fn propositions_by_note(units: &[BriefUnit]) -> HashMap<u64, crate::propositions::Proposition> {
    let read = units.iter().map(|unit| unit.unit.clone()).collect::<Vec<_>>();
    footnote_propositions(&read, QuoteContext::Passage).propositions.into_iter().map(|proposition| (proposition.footnote_id, proposition)).collect()
}

/// The citation a note cites alone, where it cites one.
fn single_source<'a>(occurrences: &'a HashMap<String, BriefOccurrence>, unit: &BriefUnit) -> Option<&'a BriefOccurrence> {
    (unit.unit.kind == "footnote" && unit.occurrence_ids.len() == 1).then(|| occurrences.get(&unit.occurrence_ids[0])).flatten()
}

/// The citations whose cited passage the review reads: each note's one citation, where its proposition marks a
/// quotation.
pub fn discrepancy_occurrences(units: &[BriefUnit], occurrences: &HashMap<String, BriefOccurrence>) -> Vec<String> {
    let propositions = propositions_by_note(units);
    units.iter().filter(|unit| unit.unit.footnote_id.and_then(|id| propositions.get(&(id as u64)))
        .is_some_and(|proposition| !quotations(&proposition.text).is_empty()))
        .filter_map(|unit| single_source(occurrences, unit)).map(|occurrence| occurrence.id.clone()).collect()
}

/// The draft's findings against the passages its citations cite (`sources`, one each; a citation given two is read
/// with none).
pub fn discrepancies(units: &[BriefUnit], occurrences: &HashMap<String, BriefOccurrence>, sources: &[CitedSource]) -> Vec<Finding> {
    let propositions = propositions_by_note(units);
    let mut by_occurrence = HashMap::<&str, Option<&CitedSource>>::new();
    for source in sources {
        by_occurrence.entry(source.occurrence_id.as_str()).and_modify(|seen| *seen = None).or_insert(Some(source));
    }
    let mut notes = units.iter().filter(|unit| unit.unit.kind == "footnote").collect::<Vec<_>>();
    notes.sort_by(|left, right| left.unit.ordinal.cmp(&right.unit.ordinal).then_with(|| left.unit.key.cmp(&right.unit.key)));
    let mut findings = Vec::new();
    for unit in notes {
        let Some(footnote) = unit.unit.footnote_id else { continue };
        let Some(occurrence) = single_source(occurrences, unit) else { continue };
        let Some(authority_id) = occurrence.authority_id.clone() else { continue };
        let (Some(Some(source)), Some(proposition)) = (by_occurrence.get(occurrence.id.as_str()), propositions.get(&(footnote as u64))) else { continue };
        if occurrence.unit_id != unit.unit.key || occurrence.pinpoints.len() != 1 || source.cited.text.trim_matches(javascript_whitespace).is_empty() {
            continue;
        }
        let evidence = [VisibleEvidenceText { evidence_id: "authorities-source".to_owned(), text: source.cited.text.clone(), labels: Vec::new() }];
        for quote in quotations(&proposition.text) {
            // Contained in the cited passage means verbatim: nothing to report.
            if quote_occurrences(&source.cited.text, &quote) > 0 { continue; }
            if grounded_prose_errors(&format!("\u{201c}{quote}\u{201d}"), &["authorities-source".to_owned()], &evidence).is_empty() { continue; }
            let (mut matches, mut found) = (0, None);
            for candidate in source.alternatives.iter().filter(|candidate| candidate.locator.kind == source.cited.locator.kind) {
                let count = quote_occurrences(&candidate.text, &quote);
                if count == 0 { continue; }
                matches += count;
                // The whole paragraph, so the card shows the quotation where the source holds it.
                if matches == 1 { found = Some(candidate); } else { found = None; break; }
            }
            let pin = &occurrence.pinpoints[0];
            let finding = |kind: &str, found: Passage, actions: Vec<&str>| Finding { id: finding_id(occurrence, source, kind, &quote), kind: kind.to_owned(),
                actions: actions.into_iter().map(str::to_owned).collect(), occurrence_id: occurrence.id.clone(), authority_id: authority_id.clone(),
                footnote_id: footnote, citation: occurrence.citation.clone(), proposition: proposition.text.clone(), authored_quote: quote.clone(),
                authored_pinpoint: AuthoredPinpoint { kind: pin.kind.clone(), text: pin.text.clone(),
                    first: pin.first.clone().filter(|first| !first.is_empty()), last: pin.last.clone().filter(|last| !last.is_empty()) },
                cited: source.cited.clone(), found: Some(found), pinpoint: None, extended: None, diff: None };
            match found {
                Some(found) if matches == 1 && !same_locator(&source.cited.locator, &found.locator) => {
                    let pinpoint = pinpoint_correction(occurrence, &found.locator.label, false).map(|change| change.3);
                    let extended = pinpoint_correction(occurrence, &found.locator.label, true).map(|change| change.3);
                    let actions = [extended.as_ref().map(|_| "pinpoint_extend"), pinpoint.as_ref().map(|_| "pinpoint"), Some("ignore")].into_iter().flatten().collect();
                    findings.push(Finding { pinpoint, extended, ..finding("wrong_pinpoint", found.clone(), actions) });
                }
                None if matches == 0 => {
                    // Words the cited passage holds otherwise; words found nowhere (scare quotes, a defined term) are none.
                    let Some(repair) = verbatim_repair(&quote, &source.cited.text) else { continue };
                    let actions = if quote_target(units, &propositions, footnote, &quote).is_some() { vec!["ignore", "quote_exact", "quote_editorial"] } else { vec!["ignore"] };
                    let diff = Some(quotation_diff(&quote, &repair));
                    findings.push(Finding { diff, ..finding("quote_mismatch", Passage { text: repair, ..source.cited.clone() }, actions) });
                }
                _ => {}
            }
        }
    }
    findings
}

/// A correction as the changes Word shows, each word it changes its own ("danger" for "dangers", not the quotation),
/// read against its unit's `text`; none where the text no longer holds what it corrects.
pub fn correction_words(text: &str, correction: &Correction) -> Option<Vec<Correction>> {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+|\s+|[^\p{L}\p{N}\s]").expect("token"));
    let units = text.encode_utf16().collect::<Vec<_>>();
    let span = units.get(correction.start..correction.end).filter(|_| correction.start < correction.end && !correction.replacement.is_empty())?;
    if String::from_utf16_lossy(span) != correction.expected { return None; }
    let tokens = |value: &str| TOKEN.find_iter(value).map(|token| token.as_str().to_owned()).collect::<Vec<_>>();
    let (left, right) = (tokens(&correction.expected), tokens(&correction.replacement));
    let offsets = std::iter::once(0).chain(left.iter().scan(0, |at, token| { *at += utf16(token); Some(*at) })).collect::<Vec<_>>();
    Some(crate::sequence_opcodes(&left, &right).into_iter().filter(|(tag, ..)| *tag != "equal").map(|(_, a0, a1, b0, b1)| Correction {
        unit_id: correction.unit_id.clone(), start: correction.start + offsets[a0], end: correction.start + offsets[a1],
        expected: left[a0..a1].concat(), replacement: right[b0..b1].concat() }).collect())
}

/// What a decision on a finding writes into the brief: the pinpoint rewritten or extended, or the quotation made the
/// source's words exactly or with the brief's editorial marks kept; none for "ignore", or where the brief no longer
/// holds what the finding read.
pub fn discrepancy_correction(units: &[BriefUnit], occurrences: &HashMap<String, BriefOccurrence>, finding: &Finding, action: &str) -> Option<Correction> {
    if action == "ignore" || !finding.actions.iter().any(|offered| offered == action) { return None; }
    let found = finding.found.as_ref()?;
    match (action, finding.kind.as_str()) {
        ("pinpoint" | "pinpoint_extend", "wrong_pinpoint") => {
            let occurrence = occurrences.get(&finding.occurrence_id)?;
            let (start, end, expected, replacement) = pinpoint_correction(occurrence, &found.locator.label, action == "pinpoint_extend")?;
            Some(Correction { unit_id: occurrence.unit_id.clone(), start, end, expected, replacement })
        }
        ("quote_exact" | "quote_editorial", "quote_mismatch") if verbatim_repair(&finding.authored_quote, &found.text).is_some() => {
            let target = quote_target(units, &propositions_by_note(units), finding.footnote_id, &finding.authored_quote)?;
            let replacement = if action == "quote_exact" { found.text.clone() } else { editorial_quote(&finding.authored_quote, &found.text) };
            Some(Correction { replacement, ..target })
        }
        _ => None,
    }
}
