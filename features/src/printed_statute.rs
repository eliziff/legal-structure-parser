//! How current a statute's print says it is.

/// How current a statute's print says it is, in its publisher's words from its first pages
/// ("Current to September 21, 2026", "Current as of April 1, 2023", "Current from 1 June 2026 to
/// 30 Sept. 2026"), capitalized; None where it says nothing.
pub fn statute_currency(pages: &[String]) -> Option<String> {
    const DATE: &str = r"(?:[A-Z][a-z]+\.?\s+[0-9]{1,2},\s+[0-9]{4}|[0-9]{1,2}\s+[A-Z][a-z]+\.?\s+[0-9]{4}|[0-9]{4}-[0-9]{2}-[0-9]{2})";
    static CURRENCY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(&format!(
        r"(?i)(?-u:\b)current\s+(?:(?:to|as\s+of)\s*:?\s*{DATE}|from\s+{DATE}\s+to\s+{DATE})")).unwrap());
    let text = legal_structure::normalize_javascript_whitespace(&pages.iter().take(3).map(String::as_str).collect::<Vec<_>>().join("\n"));
    let found = CURRENCY.find(&text)?.as_str();
    let mut characters = found.chars();
    Some(characters.next().into_iter().flat_map(char::to_uppercase).chain(characters).collect())
}
