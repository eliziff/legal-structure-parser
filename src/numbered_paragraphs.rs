//! A document's printed paragraph numbers ("[12]"), the editorial lines a quotation sets under it, and each
//! number's paragraph as every consumer addresses it (`DocumentStructure::paragraph_printings`). Which numbers are the document's own
//! and which a quotation's is the paragraph reader's one count (inference/paragraphs.rs).

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
pub fn paragraph_number(text: &str) -> Option<usize> {
    bracket_label(text).filter(|number| *number < 1000)
}

/// A quotation's editorial line, wholly in square brackets or an ellipsis alone: "[Emphasis
/// added]", "[Citations have been omitted.]", "[…]", "…", ". . .". A bracketed paragraph number is
/// none.
pub fn editorial_line(text: &str) -> bool {
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
