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
