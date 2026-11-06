//! A document's printed paragraph numbers ("[12]"), the editorial lines a quotation sets under it, and each
//! number's paragraph as every consumer addresses it (`paragraph_printings`). Which numbers are the document's own
//! and which a quotation's is the paragraph reader's one count (inference/paragraphs.rs).

use legal_structure_model::{DocumentStructure, NodeKind};
use std::collections::{BTreeMap, HashSet};

/// Each paragraph number the structure labels ("par12"), once, with the nodes printing it, the first in the text
/// first: a parallel report prints a paragraph once per language, side by side, so its printings are those on a
/// page of the first, or, where the report sets its languages or decisions one after the other, those the parser
/// marks as printing it again; the same number on other pages is another count's (a list of the issues in the other
/// language), and the number is the first's.
pub fn paragraph_printings(document: &DocumentStructure) -> Vec<(usize, Vec<usize>)> {
    let translated = |index: usize| document.nodes[index].grammar.as_deref() == Some("translation");
    let mut by_number = BTreeMap::<usize, Vec<usize>>::new();
    for (index, node) in document.nodes.iter().enumerate().filter(|(_, node)| node.kind == NodeKind::Paragraph) {
        let number = node.label.as_deref().and_then(|label| label.strip_prefix("par")).and_then(|n| n.parse::<usize>().ok());
        if let Some(number) = number {
            by_number.entry(number).or_default().push(index);
        }
    }
    by_number
        .into_iter()
        .map(|(number, mut printings)| {
            printings.sort_by_key(|&index| document.query_range(&document.nodes[index]).start);
            let first: HashSet<usize> = document.nodes[printings[0]].page_indexes.iter().copied().collect();
            let lead = printings[0];
            printings.retain(|&index| index == lead || translated(index) || document.nodes[index].page_indexes.iter().any(|page| first.contains(page)));
            (number, printings)
        })
        .collect()
}

/// "[12]" opening a line, before its words or alone: a printed paragraph number in the body.
fn bracket_label(text: &str) -> Option<usize> {
    let (number, rest) = text.trim_start().strip_prefix('[')?.split_once(']')?;
    let number = number.trim();
    if (!rest.is_empty() && !rest.starts_with(char::is_whitespace))
        || number.is_empty()
        || !number.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    number.parse().ok()
}

/// A body line's bracketed paragraph number ("[12] ..."); a bracketed year ("[2008] 2 S.C.R. 3")
/// numbers no paragraph.
pub(crate) fn paragraph_number(text: &str) -> Option<usize> {
    bracket_label(text).filter(|number| *number < 1000)
}

/// A quotation's editorial line, wholly in square brackets or an ellipsis alone: "[Emphasis
/// added]", "[Citations have been omitted.]", "[…]", "…", ". . .". A bracketed paragraph number is
/// none.
pub(crate) fn editorial_line(text: &str) -> bool {
    let text = text.trim();
    let bracketed = text.starts_with('[')
        && text.trim_end_matches(['.', ',', ';']).ends_with(']')
        && text[1..].find(']').is_some_and(|close| close + 2 >= text.trim_end_matches(['.', ',', ';']).len())
        && bracket_label(text).is_none();
    let ellipsis = !text.is_empty()
        && text.chars().all(|c| matches!(c, '.' | '\u{2026}' | '[' | ']' | '(' | ')') || c.is_whitespace())
        && (text.contains('\u{2026}') || text.matches('.').count() >= 3);
    // A remark in parentheses alone on its line: "(sic)", "(emphasis added)", "(c’est nous qui soulignons)", "(page 527)".
    let lower = text.to_lowercase();
    let remark = text.starts_with('(') && text.trim_end_matches('.').ends_with(')') && text.matches('(').count() == 1
        && ["sic)", "emphasis", "underlin", "italics", "soulign", "omitted", "ajouté", "page ", "para", "p. ", "pp. "].iter().any(|word| lower.contains(word));
    bracketed || ellipsis || remark
}
