//! The canonical spellings of a document's printed locators, shared by the structure engine and the queries over it,
//! neither of which can depend on the other.

use legal_text::{javascript_whitespace, trim_javascript_whitespace as js_trim, JS_WHITESPACE_CLASS as JS_WS};
use regex::Regex;
use std::sync::OnceLock;

pub fn literal_page_marker(line: &str, insensitive: bool) -> Option<&str> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let marker = line.strip_suffix(']')?;
    let prefix = marker.get(..6)?;
    ((insensitive && prefix.eq_ignore_ascii_case("[page ")) || prefix == "[page ")
        .then(|| &marker[6..])
}

pub fn compact_provision_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !javascript_whitespace(*character))
        .collect()
}

/// A section's place as a link names it ("sec5(2)", "secIV"), or a heading's by its title ("sectitle:general
/// provisions"), from a citation's locator ("s. 5 (2)", "section IV", "sec5(2)"); empty where it names neither.
pub fn normalize_section_locator(locator: &str) -> String {
    match section_locator(locator) {
        Some(SectionLocator::Section(label)) => format!("sec{label}"),
        Some(SectionLocator::Title(title)) => format!("sectitle:{title}"),
        None => String::new(),
    }
}

/// What a citation's section locator names: a section by its label as the structure holds it ("5(2)", "IV"), or a
/// heading by its title, its words lowercased and spaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SectionLocator {
    Section(String),
    Title(String),
}

/// A citation's section locator read once ("s. 5 (2)", "section IV", "sec5(2)", "General Provisions").
pub fn section_locator(locator: &str) -> Option<SectionLocator> {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    static NON_TITLE: OnceLock<Regex> = OnceLock::new();
    let without_prefix = section_words_off(locator);
    if let Some(label) = numbered_section_label(&compact_provision_label(&without_prefix)) {
        return Some(SectionLocator::Section(label));
    }
    let heading = without_prefix
        .trim_end_matches(|character| character == '.' || javascript_whitespace(character));
    if HEADING
        .get_or_init(|| Regex::new(r"^(?:[IVXLCDM]+|[A-Z])$").expect("valid heading grammar"))
        .is_match(heading)
    {
        return Some(SectionLocator::Section(heading.to_owned()));
    }
    let lowercase = heading.to_lowercase();
    let normalized = NON_TITLE
        .get_or_init(|| Regex::new(r"[^\p{L}\p{N}]+").expect("valid section title grammar"))
        .replace_all(&lowercase, " ");
    let title = js_trim(&normalized);
    (!title.is_empty()).then(|| SectionLocator::Title(title.to_owned()))
}

/// A section locator with the words it may open on taken off: "s. 5 (2)", "section IV", "sec5(2)" are "5 (2)", "IV",
/// "5(2)"; "secSchedule 1" is "Schedule 1".
fn section_words_off(locator: &str) -> String {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    static CANONICAL_PREFIX: OnceLock<Regex> = OnceLock::new();
    let prefix = PREFIX.get_or_init(|| Regex::new(&[r"(?iu)^(?:sections?|ss?\.?)", JS_WS, "+"].concat()).expect("section prefix"));
    let canonical = CANONICAL_PREFIX.get_or_init(|| Regex::new(r"(?iu)^sec([\p{L}\p{N}])").expect("canonical section"));
    canonical.replace(&prefix.replace(js_trim(locator), ""), "$1").into_owned()
}

/// A typed locator of a kind as the label it names is written, the words it may open on taken off: "para. 12" and
/// "par12" are "12", "pp. 4-6" is "4-6", "fn 3" is "3", a provision's "s. 5(2)" and "sec5(2)" are "5(2)".
pub fn bare_label(kind: &str, raw: &str) -> String {
    match kind {
        "section" => match section_locator(raw) {
            Some(SectionLocator::Section(label)) => label,
            _ => js_trim(&section_words_off(raw)).to_owned(),
        },
        "paragraph" | "page" | "line" | "footnote" => bare(kind, raw),
        _ => js_trim(raw).to_owned(),
    }
}

/// A numbered section's place ("sec33.1(2)") from its compact label, whitespace already out; empty where it is none.
pub fn normalize_compact_numbered_section_locator(compact: &str) -> String {
    numbered_section_label(compact).map_or_else(String::new, |label| format!("sec{label}"))
}

/// A numbered section's label ("33.1(2)", "A-1") where a compact label is one.
pub fn numbered_section_label(compact: &str) -> Option<String> {
    static NUMERIC: OnceLock<Regex> = OnceLock::new();
    static ALPHANUMERIC: OnceLock<Regex> = OnceLock::new();
    let numeric = NUMERIC.get_or_init(|| {
        Regex::new(r"^\d{1,8}[A-Za-z]{0,3}(?:[.-]\d{1,8}[A-Za-z]{0,3}){0,3}(?:\([^)]+\))*$")
            .expect("valid numeric section locator grammar")
    });
    let alphanumeric = ALPHANUMERIC.get_or_init(|| {
        Regex::new(r"^[A-Za-z]{1,3}(?:[.-][0-9A-Za-z]{1,8}){1,3}(?:\([^)]+\))*$")
            .expect("valid alphanumeric section locator grammar")
    });
    (numeric.is_match(compact) || alphanumeric.is_match(compact)).then(|| compact.to_owned())
}

/// A typed locator of a kind with the words it may open on taken off, as the number or label it names is written:
/// "para. 12", "¶ 12", "[12]", "#par12" are "12"; "pp. 4-6" is "4-6"; "fn 3", "note #3" are "3". `kind`: `paragraph`,
/// `page`, `line` or `footnote`.
fn bare(kind: &str, raw: &str) -> String {
    static PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    static PAGE: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    static FOOTNOTE: OnceLock<Regex> = OnceLock::new();
    use unicode_normalization::UnicodeNormalization;
    let value = raw.nfkc().collect::<String>();
    let value = value.trim().trim_start_matches('#').trim();
    let value = value.strip_prefix('[').and_then(|value| value.strip_suffix(']')).unwrap_or(value).trim();
    let words = match kind {
        "page" => PAGE.get_or_init(|| Regex::new(r"(?i)^(?:pages?|pp?)\.?[\s:_=-]*").unwrap()),
        "line" => LINE.get_or_init(|| Regex::new(r"(?i)^(?:lines?|ll?)\.?[\s:_=-]*").unwrap()),
        "footnote" => FOOTNOTE.get_or_init(|| Regex::new(r"(?i)^(?:footnotes?|notes?|fn)?[\s#.:_=-]*").unwrap()),
        _ => PARAGRAPH.get_or_init(|| Regex::new(r"(?i)^(?:paragraphs?|paras?|pars?|¶)\.?[\s:_=-]*").unwrap()),
    };
    words.replace(value, "").trim().to_owned()
}

/// The number a typed paragraph, page, transcript line or note locator names ("para. 012" is 12); none where it names
/// no one number.
pub fn parse_ordinal(kind: &str, raw: &str) -> Option<usize> {
    let number = bare(kind, raw);
    (!number.is_empty() && number.len() <= 6 && number.bytes().all(|byte| byte.is_ascii_digit())).then(|| number.parse().ok()).flatten()
}

/// The first and last numbers a typed range of paragraphs, pages or lines names ("paras 142-54" runs from 142 to 154,
/// "138-9" to 139, "4 to 6" from 4 to 6).
pub fn ordinal_range(kind: &str, raw: &str) -> Option<(usize, usize)> {
    static RANGE: OnceLock<Regex> = OnceLock::new();
    let bare = bare(kind, raw);
    let captures = RANGE.get_or_init(|| Regex::new(r"(?i)^(\d{1,6})\s*(?:-|–|—|\.\.|to)\s*(\d{1,6})$").unwrap()).captures(&bare)?;
    // An end shorter than its start keeps the start's leading figures.
    let (first, end) = (&captures[1], &captures[2]);
    let last = if end.len() < first.len() { format!("{}{end}", &first[..first.len() - end.len()]) } else { end.to_owned() };
    Some((first.parse().ok()?, last.parse().ok()?))
}

/// Each numbered label of `kind` (a paragraph's, page's or line's) as the numbers it names, a run written out ("12–14"
/// is 12, 13, 14; "paras 142-54" runs to 154): the inverse of `collapse_labels`. None for a label naming no number or
/// run, or a run reversed or spanning more than `limit` past its first number.
pub fn expand_labels(labels: &[String], kind: &str, limit: usize) -> Vec<Option<Vec<String>>> {
    labels.iter().map(|label| {
        let (first, last) = ordinal_range(kind, label).or_else(|| parse_ordinal(kind, label).map(|number| (number, number)))?;
        (first <= last && last - first <= limit).then(|| (first..=last).map(|number| number.to_string()).collect())
    }).collect()
}

/// A typed note locator as a note's label is written ("fn 3" is "3", "note *" is "*"), lowercased.
pub fn note_label(raw: &str) -> String {
    let label = bare("footnote", raw).to_lowercase();
    label.parse::<usize>().map_or(label, |number| number.to_string())
}

/// The words a label of any kind may open on, as a reader collapses labels: "s. 5(2)", "paras 12", "fn 3".
fn label_words() -> &'static Regex {
    static WORDS: OnceLock<Regex> = OnceLock::new();
    WORDS.get_or_init(|| {
        Regex::new(r"(?iu)^(?:sections?|secs?|ss?|s|paragraphs?|paras?|par|pages?|pp?|footnotes?|notes?|nn?|fn)\.?[\s._=-]*").unwrap()
    })
}

fn is_dash(character: char) -> bool {
    matches!(character, '\u{2013}' | '\u{2014}' | '-')
}

/// A provision's label as collapsing reads it: its root ("5", "A1", "33.1") and its parts ("(2)", "(a)").
#[derive(Clone, PartialEq)]
struct Provision {
    root: String,
    parts: Vec<String>,
}

impl Provision {
    fn read(label: &str) -> Option<Self> {
        static PROVISION: OnceLock<Regex> = OnceLock::new();
        static PART: OnceLock<Regex> = OnceLock::new();
        let label = label_words().replace(label, "");
        let found = PROVISION.get_or_init(|| Regex::new(r"^([A-Za-z]?\d+(?:\.\d+)*)((?:\([^()\[\]]{1,12}\))*)$").unwrap()).captures(&label)?;
        let parts = PART.get_or_init(|| Regex::new(r"\([^()\[\]]{1,12}\)").unwrap())
            .find_iter(&found[2]).map(|part| part.as_str().to_owned()).collect();
        Some(Self { root: found[1].to_owned(), parts })
    }

    fn text(&self) -> String {
        [self.root.clone()].into_iter().chain(self.parts.iter().cloned()).collect()
    }

    /// Whether `child` is a part of this provision: the same root, its parts beginning with these.
    fn holds(&self, child: &Self) -> bool {
        self.root == child.root && self.parts.len() < child.parts.len() && self.parts.iter().zip(&child.parts).all(|(own, theirs)| own == theirs)
    }

    /// Whether `next` is the part after this one at the same depth: "(2)" after "(1)", "(b)" after "(a)", "(1.1)" after "(1)".
    fn precedes(&self, next: &Self) -> bool {
        let depth = self.parts.len();
        if depth == 0 || depth != next.parts.len() || self.parts[..depth - 1] != next.parts[..depth - 1] {
            return false;
        }
        let (a, b) = (&self.parts[depth - 1][1..self.parts[depth - 1].len() - 1], &next.parts[depth - 1][1..next.parts[depth - 1].len() - 1]);
        let dotted = |value: &str| !value.is_empty() && value.split('.').all(|piece| !piece.is_empty() && piece.bytes().all(|byte| byte.is_ascii_digit()));
        if dotted(a) && dotted(b) {
            let from = a.split('.').map(|piece| piece.parse::<f64>().unwrap_or(f64::NAN)).collect::<Vec<_>>();
            let to = b.split('.').map(|piece| piece.parse::<f64>().unwrap_or(f64::NAN)).collect::<Vec<_>>();
            return b == format!("{a}.1") || to.len() <= from.len() && to[..to.len() - 1] == from[..to.len() - 1]
                && to[to.len() - 1] == from[to.len() - 1] + 1.0;
        }
        let letter = |value: &str| value.len() == 1 && value.bytes().all(|byte| byte.is_ascii_lowercase());
        letter(a) && letter(b) && b.as_bytes()[0] == a.as_bytes()[0] + 1
    }
}

/// Provisions written as one span: their shared parts once, then the first's and the last's own ("5(2)(a)–(c)").
fn provision_span(labels: &[String]) -> Option<String> {
    let parsed = labels.iter().map(|label| Provision::read(label)).collect::<Option<Vec<_>>>()?;
    let mut unique: Vec<Provision> = Vec::new();
    for provision in parsed {
        if let Some(known) = unique.iter_mut().find(|known| known.text() == provision.text()) { *known = provision; } else { unique.push(provision); }
    }
    let minimal = unique.iter().enumerate().filter(|(at, value)| !unique.iter().enumerate().any(|(other, holder)| other != *at && holder.holds(value)))
        .map(|(_, value)| value.clone()).collect::<Vec<_>>();
    if minimal.len() == 1 {
        return Some(minimal[0].text());
    }
    let paths = minimal.iter().map(|value| [value.root.clone()].into_iter().chain(value.parts.iter().cloned()).collect::<Vec<_>>()).collect::<Vec<_>>();
    let mut shared = 0;
    while paths.iter().all(|path| path.len() > shared + 1) && paths.iter().all(|path| path[shared] == paths[0][shared]) {
        shared += 1;
    }
    (shared > 0).then(|| format!("{}{}\u{2013}{}", paths[0][..shared].concat(), paths[0][shared..].concat(), paths[paths.len() - 1][shared..].concat()))
}

fn provisions(labels: &[String]) -> Option<Vec<String>> {
    let mut unique: Vec<String> = Vec::new();
    for label in labels {
        if !unique.contains(label) { unique.push(label.clone()); }
    }
    let ends = |label: &str| label.split(is_dash).map(str::to_owned).collect::<Vec<_>>();
    if unique.len() == 1 && unique[0].contains(is_dash) {
        let endpoints = ends(&unique[0]).iter().map(|end| Provision::read(end)).collect::<Option<Vec<_>>>()?;
        let texts = endpoints.iter().map(Provision::text).collect::<Vec<_>>();
        return Some(vec![provision_span(&texts).unwrap_or_else(|| texts.join("\u{2013}"))]);
    }
    let values = unique.iter().flat_map(|label| ends(label)).map(|end| Provision::read(&end)).collect::<Option<Vec<_>>>()?;
    if unique.iter().any(|label| label.contains(is_dash)) {
        let parents = unique.iter().filter(|label| !label.contains(is_dash)).filter_map(|label| Provision::read(label)).collect::<Vec<_>>();
        return Some(unique.iter().filter(|label| !parents.iter().any(|parent| ends(label).iter().all(|end| Provision::read(end).is_some_and(|end| parent.holds(&end)))))
            .flat_map(|label| provisions(std::slice::from_ref(label)).unwrap_or_default()).collect());
    }
    let minimal = values.iter().enumerate().filter(|(at, value)| !values.iter().enumerate().any(|(other, holder)| other != *at && holder.holds(value)))
        .map(|(_, value)| value.clone()).collect::<Vec<_>>();
    let mut groups: Vec<(String, Vec<Provision>)> = Vec::new();
    for value in minimal {
        match groups.iter_mut().find(|(root, _)| *root == value.root) {
            Some((_, group)) => group.push(value),
            None => groups.push((value.root.clone(), vec![value])),
        }
    }
    let mut spans = Vec::new();
    for (_, group) in groups {
        let mut index = 0;
        while index < group.len() {
            let first = &group[index];
            let mut last = first;
            while index + 1 < group.len() && last.precedes(&group[index + 1]) {
                index += 1;
                last = &group[index];
            }
            spans.push(if first == last { first.text() } else { provision_span(&[first.text(), last.text()])? });
            index += 1;
        }
    }
    Some(spans)
}

/// Numbered labels as runs: "12", "13", "14", "20" are "12–14", "20".
fn numbered_runs(labels: &[String]) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for label in labels {
        let value = label_words().replace(label, "").trim().to_owned();
        if !value.is_empty() && value.len() <= 6 && value.bytes().all(|byte| byte.is_ascii_digit()) {
            let number = value.parse::<u64>().ok()?;
            if !values.contains(&number) { values.push(number); }
        } else {
            return None;
        }
    }
    values.sort_unstable();
    let mut runs = Vec::new();
    let mut index = 0;
    while index < values.len() {
        let start = values[index];
        let mut end = start;
        while values.get(index + 1) == Some(&(end + 1)) {
            index += 1;
            end = values[index];
        }
        runs.push(if start == end { start.to_string() } else { format!("{start}\u{2013}{end}") });
        index += 1;
    }
    Some(runs)
}

/// A reader's labels of one kind written as few as they can be: paragraphs, pages and notes as runs of numbers,
/// provisions as their spans with what a span or a holder already covers left out. None where a label is not of the
/// kind's shape, or the kind has no such form.
pub fn collapse_labels(labels: &[String], kind: &str) -> Option<Vec<String>> {
    if labels.is_empty() {
        return None;
    }
    match kind {
        "paragraph" | "page" | "footnote" => numbered_runs(labels),
        "section" => provisions(labels),
        _ => None,
    }
}

