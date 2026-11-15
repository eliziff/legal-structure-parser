//! What a footnote's quotations are checked against (the ALR Quote Verifier's "quote context"): its proposition, read by
//! the model's one proposition reader. Offsets count UTF-16 units, as the host's text does.

use legal_structure_model::AuthorityTextUnit;
use serde::{Deserialize, Serialize};

/// How much of the text a note's proposition is.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum QuoteContext {
    Passage,
    Sentence,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct PropositionPart {
    /// The unit's place among those given, and the part of its text the proposition holds.
    pub unit: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Proposition {
    pub footnote_id: u64,
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub parts: Vec<PropositionPart>,
}

/// The body text's propositions, where each body unit starts in it (its units in order, each followed by a line
/// break; none for a note's unit), and every call of a note in it by where it stands.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FootnotePropositions {
    pub propositions: Vec<Proposition>,
    pub body_starts: Vec<Option<usize>>,
    pub anchors: Vec<(u64, usize)>,
}

/// Each note's proposition in the body units (the model's one reading of it): with `context` sentence, the sentence its
/// call ends or stands in; else the passage since the call before it.
pub fn footnote_propositions(units: &[AuthorityTextUnit], context: QuoteContext) -> FootnotePropositions {
    let mut body = (0..units.len()).filter(|&index| units[index].kind == "body").collect::<Vec<_>>();
    body.sort_by(|&a, &b| units[a].ordinal.cmp(&units[b].ordinal).then_with(|| units[a].key.cmp(&units[b].key)));
    let texts = body.iter().map(|&index| units[index].text.as_str()).collect::<Vec<_>>();
    let calls = body.iter().enumerate().flat_map(|(at, &index)| units[index].footnote_refs.iter()
        .filter(|&&(id, _)| id > 0).map(move |&(id, offset)| (at, id as u64, offset))).collect::<Vec<_>>();
    let read = legal_structure_model::propositions::propositions(&texts, &calls);
    let mut body_starts = vec![None; units.len()];
    let parts = body.iter().zip(&read.unit_starts).map(|(&index, &start)| {
        body_starts[index] = Some(start);
        (index, start, units[index].text.encode_utf16().count())
    }).collect::<Vec<_>>();
    let propositions = read.notes.into_iter().filter_map(|note| {
        let ((start, end), text) = match context {
            QuoteContext::Sentence => (note.sentence, note.sentence_text),
            QuoteContext::Passage => (note.passage, note.passage_text),
        };
        let parts = parts.iter().filter_map(|&(unit, offset, length)| {
            let (from, to) = (start.saturating_sub(offset), length.min(end.saturating_sub(offset)));
            (offset < end && from < to).then_some(PropositionPart { unit, start: from, end: to })
        }).collect();
        (start < end).then_some(Proposition { footnote_id: note.note, text, start, end, parts })
    }).collect();
    FootnotePropositions { propositions, body_starts, anchors: read.calls }
}

/// The quotations a brief's units give to check, and the citation parts each is checked against: read with the marked
/// quotations' reader (quote verification).
#[cfg(feature = "quote-verification")]
mod quotations {
    use super::*;

    /// How far past a quotation a citation in its own unit may start and still be the quotation's (UTF-16 units).
    const QUOTED_CITATION_REACH: usize = 400;

    /// A unit's citations as its quotations' sources are read from them: where each of its citations starts, with the span
    /// its authority's citation covers (a quotation inside one is the cited work's title, no quotation), and the parts its
    /// text splits into as a note's citation parts. UTF-16 offsets in the unit's text.
    #[derive(Deserialize, Default)]
    #[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
    pub struct UnitCitations {
        pub occurrences: Vec<UnitCitation>,
        pub parts: Vec<[usize; 2]>,
    }

    #[derive(Deserialize)]
    #[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
    pub struct UnitCitation {
        pub start: usize,
        pub cited: Option<[usize; 2]>,
    }

    /// A citation part a quotation is checked against: its unit's place among those given, the part's place among the
    /// unit's, and the unit's citations starting in it that cite for the quotation, by their place among the unit's.
    #[derive(Serialize)]
    #[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
    pub struct QuotationCandidate {
        pub unit: usize,
        pub part: usize,
        pub occurrences: Vec<usize>,
    }

    /// A quotation to check: its unit's place among those given, the quoted words and where they stand in the unit's text
    /// (UTF-16), and the citation parts it is checked against.
    #[derive(Serialize)]
    #[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
    pub struct PropositionQuotation {
        pub unit: usize,
        pub start: usize,
        pub end: usize,
        pub text: String,
        pub candidates: Vec<QuotationCandidate>,
    }

    /// The quotations to check in the units, each with the citation parts it is checked against (the quote and proposition
    /// model). A quotation in the body belongs to the note whose call ends the passage it is in, the next call after it,
    /// and only where the note's proposition (`context`) holds it; it is checked against that note's parts. One with no
    /// note after it, or in a note's own text, is checked against the parts of its own unit that a citation starting
    /// within reach after it cites. A quotation inside a citation's span is that citation's quoted title. With `text_only`,
    /// only the body's quotations are read, none a note holds. `citations` is each unit's, in the units' order.
    pub fn proposition_quotations(units: &[AuthorityTextUnit], citations: &[UnitCitations], context: QuoteContext,
        text_only: bool) -> Vec<PropositionQuotation> {
        let read = footnote_propositions(units, context);
        let passages = read.propositions.iter().map(|proposition| (proposition.footnote_id, proposition))
            .collect::<std::collections::HashMap<_, _>>();
        let none = UnitCitations::default();
        let cited = |index: usize| citations.get(index).unwrap_or(&none);
        let mut quotations = Vec::new();
        for (index, unit) in units.iter().enumerate() {
            if text_only && unit.kind != "body" {
                continue;
            }
            let own = cited(index);
            for quote in crate::quote_verification::marked_quote_spans(&unit.text) {
                if own.occurrences.iter().any(|item| item.cited.is_some_and(|[start, end]| start <= quote.start && quote.end <= end)) {
                    continue;
                }
                let offset = read.body_starts.get(index).copied().flatten();
                let end = offset.unwrap_or(0) + quote.end;
                let anchor = offset.and_then(|_| read.anchors.iter().find(|&&(_, at)| at >= end));
                if let Some(passage) = anchor.and_then(|(note, _)| passages.get(note)) {
                    if end - quote.end + quote.start < passage.start || end > passage.end {
                        continue;
                    }
                }
                // The note after the quotation, by its id and where its call stands in the quotation's unit.
                let next = anchor.map(|&(note, at)| (note, at - offset.unwrap_or(0)));
                let notes = next.map(|(note, _)| units.iter().enumerate()
                    .filter(|(_, item)| item.kind == "footnote" && item.footnote_id.map(|id| id as u64) == Some(note))
                    .map(|(at, _)| at).collect::<Vec<_>>()).unwrap_or_default();
                // The quotation's own unit's citations that start within reach after it, before the next note's call.
                let following = own.occurrences.iter().enumerate().filter(|(_, item)| item.start >= quote.end
                    && item.start - quote.end <= QUOTED_CITATION_REACH && next.is_none_or(|(_, call)| item.start < call))
                    .map(|(at, _)| at).collect::<Vec<_>>();
                let sources = if notes.is_empty() { vec![index] } else { notes };
                let candidates = sources.into_iter().flat_map(|source| {
                    let theirs = cited(source);
                    let citing = if source == index { following.clone() } else { (0..theirs.occurrences.len()).collect() };
                    theirs.parts.iter().enumerate().filter_map(|(part, &[from, to])| {
                        let occurrences = citing.iter().copied()
                            .filter(|&at| (from..to).contains(&theirs.occurrences[at].start)).collect::<Vec<_>>();
                        (source != index || to > quote.end && from <= quote.end + QUOTED_CITATION_REACH && !occurrences.is_empty())
                            .then_some(QuotationCandidate { unit: source, part, occurrences })
                    }).collect::<Vec<_>>()
                }).collect();
                quotations.push(PropositionQuotation { unit: index, start: quote.start, end: quote.end, text: quote.text,
                    candidates });
            }
        }
        quotations
    }
}
#[cfg(feature = "quote-verification")]
pub use quotations::*;
