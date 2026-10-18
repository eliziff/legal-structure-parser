//! A statute's outline read from its text: its parts, divisions, headings and schedules, and
//! each provision with its marginal note and its own text, in document order, each covering
//! its whole subtree, so a caller can lay the statute out or cut one provision out of it.

use crate::analysis::citation_spans;
use crate::{
    provider_text_document_structure, EngineError, NodeKind, ProviderTextInput,
    ProviderTextSourceKind,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatuteOutlineEntry {
    /// "preamble" (the words before the first heading or provision: a long title, an enacting
    /// formula), "part", "division", "heading", "schedule", "section" (or "article" in a code
    /// numbered by article), "subsection", "paragraph", "subparagraph" or "clause".
    pub kind: &'static str,
    /// Headings nest by their own hierarchy, a provision sits under the heading before it,
    /// and each enumerated provision under the one it is part of.
    pub level: usize,
    /// The number as printed: "726", "726.1", "1-3", "(2)", "(a)", "PART XXIII"; empty for a
    /// heading without one.
    pub label: String,
    /// A provision's marginal note, or a heading's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The entry's own text, before any entry under it, without its label or the source's
    /// emphasis marks: a line to each of its paragraphs.
    pub text: String,
    /// The entry's span in the input, in UTF-16 code units, covering everything under it; a
    /// provision's span opens with its marginal note.
    pub start: usize,
    pub end: usize,
}

/// A line of the text: its scalar span and its content.
struct TextLine<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

/// A heading line's kind, label and title: "PART XXIII – Sentencing" is a part labelled
/// "PART XXIII" titled "Sentencing".
fn heading(text: &str) -> (&'static str, String, Option<String>) {
    let mut words = text.split_whitespace();
    let first = words.next().unwrap_or_default();
    let kind = match first.to_lowercase().as_str() {
        "part" | "partie" => "part",
        "division" => "division",
        "schedule" | "annexe" | "appendix" => "schedule",
        _ => "heading",
    };
    if kind == "heading" {
        return (kind, String::new(), Some(text.to_owned()));
    }
    // Its number: digits, a Roman numeral or a letter ("PART XXIII", "Part 2", "Division A").
    let number = words.next().map(|word| word.trim_end_matches(['.', ':'])).filter(|word| {
        word.chars().any(|c| c.is_ascii_digit())
            || !word.is_empty() && word.chars().all(|c| "IVXLCDM".contains(c))
            || word.chars().count() == 1 && word.chars().all(|c| c.is_ascii_uppercase())
    });
    let label = number.map_or_else(|| first.to_owned(), |number| format!("{first} {number}"));
    let rest = text.split_whitespace().skip(if number.is_some() { 2 } else { 1 }).collect::<Vec<_>>().join(" ");
    let title = rest.trim_start_matches(['–', '—', '-', ':', '.', ' ']).trim();
    (kind, label, (!title.is_empty()).then(|| title.to_owned()))
}

/// What a provision's enumerators make it, by how they nest: "(2)" a subsection, "(a)" a
/// paragraph, "(i)" under a paragraph a subparagraph, "(A)" a clause.
fn enumerated_kind(tokens: &[&str]) -> &'static str {
    tokens.iter().fold("section", |parent, token| {
        if token.starts_with(|c: char| c.is_ascii_digit()) {
            "subsection"
        } else if token.starts_with(|c: char| c.is_ascii_uppercase()) {
            "clause"
        } else if matches!(parent, "paragraph" | "subparagraph") {
            "subparagraph"
        } else {
            "paragraph"
        }
    })
}

/// The text without markdown emphasis, a line to each paragraph.
fn plain(text: &str) -> String {
    text.lines()
        .map(|line| line.replace(['*', '_'], "").trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads a statute's text, as A2AJ and other providers give it (markdown headings, a marginal
/// note on its own line before its provision), into its outline. `articles` reads a code whose
/// provisions are articles (Quebec's).
pub fn statute_outline(text: &str, articles: bool) -> Result<Vec<StatuteOutlineEntry>, EngineError> {
    let provider = provider_text_document_structure(ProviderTextInput::new("", ProviderTextSourceKind::Laws, text))?;
    // Text run together, a page to a line (a PDF's page text), is read again with its lines
    // recovered where provisions open; the reading that finds more provisions is kept. The
    // recovery only breaks lines, so offsets into the text hold.
    let provisions = |structure: &crate::DocumentStructure| structure.nodes.iter()
        .filter(|node| node.kind == NodeKind::Section && node.label.as_deref().is_some_and(|label| label.starts_with("sec") && !label.contains('(')))
        .count();
    let structure = if articles || provisions(&provider) * 200 < text.len().max(1) {
        let lineated = crate::analyze_instrument(text.to_owned(), String::new(), &[], true)?;
        if provisions(&lineated) > provisions(&provider) { lineated } else { provider }
    } else {
        provider
    };
    let chars = text.chars().collect::<Vec<_>>();
    // Each scalar offset's UTF-16 offset.
    let mut utf16 = Vec::with_capacity(chars.len() + 1);
    let mut at = 0;
    for character in &chars {
        utf16.push(at);
        at += character.len_utf16();
    }
    utf16.push(at);
    let slice = |start: usize, end: usize| chars[start.min(chars.len())..end.min(chars.len())].iter().collect::<String>();
    let mut lines = Vec::new();
    let (mut scalar, mut byte) = (0, 0);
    for raw in text.split_inclusive('\n') {
        let content = raw.trim_end_matches(['\n', '\r']);
        lines.push(TextLine { start: scalar, end: scalar + content.chars().count(), text: &text[byte..byte + content.len()] });
        scalar += raw.chars().count();
        byte += raw.len();
    }
    // An entry as read: its span in scalars, where its own text opens, and whether a heading.
    struct Entry { start: usize, content: usize, end: usize, kind: &'static str, level: usize, label: String, title: Option<String>, heading: bool }
    let mut entries = Vec::<Entry>::new();
    for line in &lines {
        let trimmed = line.text.trim_start();
        let depth = trimmed.chars().take_while(|c| *c == '#').count();
        // The document's own title ("# Criminal Code") opens no part of it.
        if depth < 2 || !trimmed[depth..].starts_with(' ') { continue; }
        let (kind, label, title) = heading(&plain(&trimmed[depth..]));
        entries.push(Entry { start: line.start, content: line.end, end: chars.len(), kind, level: depth - 2, label, title, heading: true });
    }
    // A marginal note: a line all in bold just above its provision, blank lines aside.
    let note = |start: usize| {
        let index = lines.iter().rposition(|line| line.end <= start && line.start < start)?;
        let line = lines[..=index].iter().rev().find(|line| !line.text.trim().is_empty())?;
        let value = line.text.trim();
        (value.len() > 4 && value.starts_with("**") && value.ends_with("**") && !value[2..value.len() - 2].contains("**")
            && !value[2..value.len() - 2].trim().starts_with(|c: char| c.is_ascii_digit()))
            .then(|| (line.start, plain(value)))
    };
    // The notes taken: a section's first subsection, opening on the section's line, has the
    // section's note above it, not one of its own.
    let mut noted = std::collections::HashSet::new();
    for node in structure.nodes.iter().filter(|node| node.kind == NodeKind::Section) {
        let Some(label) = node.label.as_deref().and_then(|label| label.strip_prefix("sec")) else { continue };
        // A label read twice ("2(a)@2") prints as its first reading.
        let label = label.split('@').collect::<String>();
        let (number, tokens) = label.split_once('(').map_or((label.as_str(), Vec::new()), |(number, rest)| {
            (number, rest.split(')').map(|token| token.trim_start_matches('(')).filter(|token| !token.is_empty()).collect::<Vec<_>>())
        });
        let (kind, printed) = match tokens.last() {
            None => (if articles { "article" } else { "section" }, number.to_owned()),
            Some(token) => (enumerated_kind(&tokens), format!("({token})")),
        };
        // A section, and a subsection, may carry a marginal note.
        let marginal = (tokens.is_empty() || kind == "subsection").then(|| note(node.range.start)).flatten()
            .filter(|(start, _)| noted.insert(*start));
        let heading_level = entries.iter().filter(|entry| entry.heading && entry.start <= node.range.start)
            .map(|entry| entry.level + 1).last().unwrap_or(0);
        entries.push(Entry {
            start: marginal.as_ref().map_or(node.range.start, |(start, _)| *start),
            content: node.range.start, end: node.range.end, kind, level: heading_level + tokens.len(),
            label: printed, title: marginal.map(|(_, title)| title), heading: false,
        });
    }
    entries.sort_by_key(|entry| (entry.start, entry.level));
    // An entry closes where the next one not under it opens.
    for index in 0..entries.len() {
        let level = entries[index].level;
        if let Some(next) = entries[index + 1..].iter().find(|next| next.level <= level) {
            entries[index].end = entries[index].end.min(next.start);
        }
    }
    // The words before the first entry, without the document's title ("# Criminal Code") or a line
    // naming it by its citation.
    let first = entries.first().map_or(chars.len(), |entry| entry.start);
    let preamble = lines.iter()
        .filter(|line| line.end <= first && !line.text.trim_start().starts_with('#'))
        .map(|line| plain(line.text))
        .filter(|line| !line.is_empty() && !citation_spans(line).iter().any(|span| span.range == (0..line.len())))
        .collect::<Vec<_>>()
        .join(" ");
    let preamble = (!preamble.is_empty()).then(|| StatuteOutlineEntry { kind: "preamble", level: 0, label: String::new(),
        title: None, text: preamble, start: 0, end: utf16[first] });
    Ok(preamble.into_iter().chain(entries.iter().enumerate().map(|(index, entry)| {
        // The entry's own text runs to the first entry under it.
        let own_end = entries.get(index + 1).filter(|next| next.start < entry.end).map_or(entry.end, |next| next.start);
        let own = if entry.heading { String::new() } else { slice(entry.content, own_end.max(entry.content)) };
        let own = plain(&own);
        // A subsection may repeat its section's number ("5(1) The director ...").
        let own = own.strip_prefix(entry.label.as_str())
            .or_else(|| own.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-')).trim_start()
                .strip_prefix(entry.label.as_str()))
            .unwrap_or(&own).trim_start_matches('.').trim().to_owned();
        StatuteOutlineEntry { kind: entry.kind, level: entry.level, label: entry.label.clone(), title: entry.title.clone(),
            text: own, start: utf16[entry.start.min(chars.len())], end: utf16[entry.end.min(chars.len())] }
    })).collect())
}

/// Where each provision `labels` cite is in a statute's text, by its outline: a section or
/// article ("16"), a part of one ("16(3)(a)", each part a level under the last), or a range
/// ("49-51", "33 to 35") from its first provision through its last; each span (UTF-16) with
/// everything under it, in the order the text has them. A label the outline does not hold has none.
pub fn statute_provision_spans(text: &str, labels: &[String]) -> Result<Vec<[usize; 2]>, EngineError> {
    static PARTS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^[^(]+|\([^)]*\)").unwrap());
    static RANGE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\s*(?:-|\u{2013}|to)\s*").unwrap());
    let outline = statute_outline(text, false)?;
    let entry = |label: &str| {
        let compact = label.chars().filter(|character| !character.is_whitespace()).collect::<String>();
        let mut parts = PARTS.find_iter(&compact).map(|part| part.as_str());
        let top = parts.next()?;
        let mut at = outline.iter().find(|item| matches!(item.kind, "section" | "article")
            && item.label.strip_suffix('.').unwrap_or(&item.label) == top.strip_suffix('.').unwrap_or(top))?;
        for part in parts {
            at = outline.iter().find(|item| item.start >= at.start && item.end <= at.end && item.level > at.level
                && item.label == part)?;
        }
        Some(at)
    };
    let mut spans = labels.iter().filter_map(|label| {
        if let Some(whole) = entry(label) {
            return Some([whole.start, whole.end]);
        }
        let mut ends = RANGE.split(label);
        let (first, last) = (entry(ends.next().unwrap_or("")), ends.next().and_then(|to| (!to.is_empty()).then(|| entry(to)).flatten()));
        Some([first?.start, last?.end])
    }).collect::<Vec<_>>();
    spans.sort_by_key(|span| span[0]);
    Ok(spans)
}
