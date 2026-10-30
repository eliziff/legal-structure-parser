//! A statute's printed lines: the marginal notes over its provisions and the history notes under
//! them.

/// A line's words as a provider's markdown sets them, without the marks of a heading ("### Commencement
/// of order"), and whether the provider set them in bold alone ("**Target — 2050**").
pub fn markdown_line(line: &str) -> (&str, bool) {
    let value = line.trim().trim_start_matches('#').trim_start();
    let emphasized = value
        .strip_prefix("**")
        .and_then(|inner| inner.strip_suffix("**"))
        .filter(|inner| !inner.trim().is_empty() && !inner.contains('*'));
    (emphasized.map_or(value, str::trim), emphasized.is_some())
}

/// Whether a line reads as a statute's marginal note: a short sentence-case line ("Lawyer's right
/// to costs out of property recovered") of 1 to 16 words, opening with a capital, with no digit and
/// no closing punctuation. A line a provider sets in bold alone is a note whatever its words
/// ("**Target — 2050**", "**Liability of directors, officers, etc.**"). Enactment-history lines
/// carry digits, so are none.
pub fn marginal_note_shaped(line: &str) -> bool {
    let (value, emphasized) = markdown_line(line);
    (1..=16).contains(&value.split_whitespace().count())
        && (emphasized
            || value.chars().next().is_some_and(char::is_uppercase)
                && !value.ends_with(['.', ';', ':', ',', '—', '-'])
                && !value.chars().any(|character| character.is_ascii_digit()))
}

/// A history note under a provision lists the enactments that made or amended it, each a
/// year or revision and its chapter: "1991, c. 43, s. 4; 2005, c. 22, s. 20", "R.S., c. C-34,
/// s. 1", "R.S., 1985, c. 27 (1st Supp.), s. 13", "2009 c50 s7". It is references only, with
/// no word longer than "suppl."; an amendment's text ("2019, c. 25, s. 5, is replaced by")
/// is no history note.
pub fn history_note(line: &str) -> bool {
    if line.split(|c: char| !c.is_alphabetic()).any(|word| word.chars().count() > 5) {
        return false;
    }
    let revised = ["R.S.C.", "R.S.", "L.R.C.", "S.R.", "S.C."].iter()
        .find_map(|prefix| line.strip_prefix(prefix)).map(|rest| rest.trim_start_matches([',', ' ']));
    let rest = revised.unwrap_or(line);
    let year = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let rest = match year {
        4 => rest[4..].trim_start_matches([',', ' ']),
        0 if revised.is_some() => rest,
        _ => return false,
    };
    rest.starts_with("c. ") || rest.starts_with("ch. ")
        || rest.strip_prefix('c').is_some_and(|number| number.starts_with(|c: char| c.is_ascii_digit()))
}
