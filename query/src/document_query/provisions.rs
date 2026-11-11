//! The provisions and notes a typed locator names, read one way for every document a reader looks into: the engine's
//! own structure lookup (`structure_block`) and a PDF's lookup by locator (`legalpdf`'s `PdfDocument::lookup`) both
//! resolve through here.

use std::borrow::Cow;
use std::collections::HashSet;
use legal_structure_model::{provision_mark, DocumentStructure, NodeKind, StructureNode};
use super::DocumentQuery;
use legal_locator::{note_label, section_locator, SectionLocator};
use unicode_normalization::UnicodeNormalization;

/// The provisions a locator names, as `DocumentQuery::provisions` reads them.
pub enum Provisions {
    /// `order` the provisions the lookup reads among, `first..=last` the ones named in it: a single provision among
    /// every provision the document prints, or a range's outermost provisions, each with what it holds.
    Found { order: Vec<usize>, first: usize, last: usize },
    /// Each provision a locator could name, where it names more than one (or a range runs backwards).
    Ambiguous(Vec<usize>),
    NotFound,
    /// The document prints no provision of the kind asked for.
    Unavailable,
}

fn nfkc(value: &str) -> Cow<'_, str> {
    if value.is_ascii() { Cow::Borrowed(value) } else { Cow::Owned(value.nfkc().collect()) }
}

/// A provision's name as a locator and its aliases are compared: a typed section locator's place ("s. 5 (2)" is
/// "sec5(2)"), else its words spaced and lowercased.
fn section_alias(raw: &str) -> String {
    let value = nfkc(raw);
    match section_locator(&value) {
        Some(SectionLocator::Section(label)) => format!("sec{label}"),
        _ => value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase(),
    }
}

/// The place a Part, Division or Schedule takes as the instrument grammar reads its heading, its kind's mark and label
/// ("part2", "div2", "scheda": `StructureNode::fragment`): a Part's or Division's number in figures or capital roman
/// numerals. None for another kind; an empty place for a number neither reads.
fn container_label(kind: &str, locator: &str) -> Option<String> {
    match kind {
        "part" | "division" => {
            let number = locator.trim().trim_start_matches(|c: char| c.is_alphabetic() && !"IVXLCDM".contains(c)).trim();
            let value = number.parse::<u32>().ok().or_else(|| legal_structure_model::layout::roman_value(number)
                .filter(|_| number.chars().all(|c| c.is_ascii_uppercase())));
            Some(value.map_or_else(String::new, |value| format!("{}{value}", if kind == "part" { "part" } else { "div" })))
        }
        "schedule" => Some(format!("sched{}", locator.trim().to_lowercase())),
        _ => None,
    }
}

/// A heading's words, without a Part's number before them or a note's number after a year, in capitals.
fn title_words(text: &str) -> Vec<String> {
    let mut words = text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty())
        .map(str::to_uppercase).collect::<Vec<_>>();
    if words.first().is_some_and(|word| word == "PART") && words.len() > 2 {
        words.drain(..2);
    }
    let year = |word: &String| word.len() == 4 && word.chars().all(|c| c.is_ascii_digit());
    if words.len() > 1 && words.last().is_some_and(|note| note.len() <= 3 && note.chars().all(|c| c.is_ascii_digit()))
        && year(&words[words.len() - 2]) {
        words.pop();
    }
    words
}

/// Whether a typed note locator (its label as `note_label` reads it, `wanted`) names the note: by its label, or by its
/// id where the id is the reader's own (a publisher's ids are its anchor's namespace, not the label's: its "fn1" may be
/// the note labelled 2).
pub fn note_named(node: &StructureNode, wanted: &str) -> bool {
    !wanted.is_empty() && (note_label(node.label.as_deref().unwrap_or("")) == wanted
        || node.anchor.is_none() && note_label(&node.id) == wanted)
}

impl DocumentQuery {
    /// Where one instrument of a document that prints several is printed ("The Constitution Acts 1867 to 1982"): from
    /// the heading that is its title ("CONSTITUTION ACT, 1982", "PART I Canadian Charter of Rights and Freedoms", a
    /// note's number after it read past) to the next heading that is another Act's title in capitals ("CANADA ACT
    /// 1982", "LOI CONSTITUTIONNELLE DE 1867"). A contents list that names it comes before its body: the last heading
    /// that is its title opens it. None where no heading is its title, or where the document prints one Act.
    fn instrument_scope(&self, document: &DocumentStructure, instrument: &str) -> Option<(usize, usize)> {
        let wanted = title_words(instrument);
        if wanted.is_empty() {
            return None;
        }
        let text = self.text(document);
        let own_text = |node: &StructureNode| {
            let ranges = if node.ranges.is_empty() { std::slice::from_ref(&node.range) } else { &node.ranges[..] };
            ranges.iter().filter_map(|range| text.slice_utf16(range.start..range.end)).filter(|part| !part.is_empty())
                .collect::<Vec<_>>().join("\n")
        };
        // Each heading's start and own text, in the order the text prints them.
        let mut headings: Vec<(usize, String)> = document.nodes.iter()
            .filter(|node| node.kind == NodeKind::Heading || node.role == Some(legal_structure_model::PrimaryRole::Heading))
            .map(|node| (node.range.start, own_text(node)))
            .collect();
        headings.sort_by_key(|(start, _)| *start);
        // Another Act's title: a heading in capitals that ends in its year.
        let act_title = |text: &str| {
            let words = title_words(text);
            !text.chars().any(char::is_lowercase) && words.iter().any(|word| word == "ACT" || word == "LOI")
                && words.last().is_some_and(|year| year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()))
        };
        let acts = headings.iter().filter(|(_, text)| act_title(text)).map(|(_, text)| title_words(text)).collect::<HashSet<_>>();
        if acts.len() < 2 {
            return None;
        }
        let (at, (start, _)) = headings.iter().enumerate().rev().find(|(_, (_, text))| title_words(text) == wanted)?;
        let end = headings[at + 1..].iter().find(|(_, text)| act_title(text) && title_words(text) != wanted)
            .map_or(usize::MAX, |(end, _)| *end);
        Some((*start, end))
    }

    /// The document's provisions, in the order its text prints them, within `scope` where it is given: each section
    /// the query serves (labelled) and each that a provision's kind names, but a parallel translation's, which repeat
    /// the body's.
    fn provision_positions(&self, document: &DocumentStructure, scope: Option<(usize, usize)>) -> Vec<usize> {
        let served: HashSet<usize> = self.served(document, NodeKind::Section).into_iter().collect();
        let mut sections: Vec<usize> = (0..document.nodes.len())
            .filter(|&index| {
                let node = &document.nodes[index];
                node.kind == NodeKind::Section
                    && (served.contains(&index) || node.locator_kind.as_deref().is_some_and(|kind| !kind.is_empty()))
                    && node.grammar.as_deref() != Some("translation")
                    && scope.is_none_or(|(start, end)| start <= node.range.start && node.range.start < end)
            })
            .collect();
        sections.sort_by_key(|&index| document.nodes[index].range.start);
        sections
    }

    /// Whether `locator` names `node` as a provision of `kind` ("section" for any): its place, its label as a
    /// section's, an alias, its id ("section:<id>"), each as a locator is spelled (`section_alias`); or as the query
    /// names a block (`requested_label`, `resolves`: its place, label, aliases under its kind's mark, its anchor).
    fn names_provision(&self, document: &DocumentStructure, node: &StructureNode, kind: &str, requested: &str, label: &str) -> bool {
        if kind != "section" && node.locator_kind.as_deref() != Some(kind) {
            return false;
        }
        let id = (kind == "section" && !node.id.is_empty()).then(|| format!("section:{}", node.id));
        let section_label = (provision_mark(node.locator_kind.as_deref()) == "sec").then_some(node.label.as_deref()).flatten();
        let fragment = node.fragment();
        fragment.iter().map(String::as_str).chain(section_label).chain(node.aliases.iter().flatten().map(String::as_str))
            .chain(id.as_deref())
            .any(|value| section_alias(value) == requested)
            || !label.is_empty() && self.resolves(document, node, label, Some(NodeKind::Section))
    }

    /// The provisions `locator` (through `end`, where a range is asked for) names among those of `kind`, within the
    /// instrument `instrument` names where the document prints several. A Part, Division or Schedule is the one its
    /// heading's place names; a code's rules are its sections. A range ("49-51", "5-2-5-3", or a locator to an end)
    /// runs from its first provision through its last, unless a provision is itself numbered so ("1-2"): its dash is
    /// the one both ends are found at; with `ranges` unset, a locator names one provision. A label the body prints
    /// twice is ambiguous, as is a range whose last provision is printed before its first.
    pub fn provisions(&self, document: &DocumentStructure, kind: &str, locator: &str, end: Option<&str>, instrument: Option<&str>,
        ranges: bool) -> Provisions {
        let scope = instrument.and_then(|instrument| self.instrument_scope(document, instrument));
        let sections = self.provision_positions(document, scope);
        let kind = if kind == "rule" { "section" } else { kind };
        let container = container_label(kind, locator).is_some();
        if sections.is_empty()
            || kind != "section" && !container
                && !sections.iter().any(|&index| document.nodes[index].locator_kind.as_deref() == Some(kind)) {
            return Provisions::Unavailable;
        }
        let named = |locator: &str| -> Vec<usize> {
            let place = container_label(kind, locator);
            let (requested, label) = (section_alias(locator), self.requested_label(document, NodeKind::Section, locator));
            (0..sections.len()).filter(|&at| {
                let node = &document.nodes[sections[at]];
                match &place {
                    Some(place) => !place.is_empty() && node.fragment().as_deref() == Some(place.as_str()),
                    None => self.names_provision(document, node, kind, &requested, &label),
                }
            }).collect()
        };
        let (first, last) = match end.filter(|end| ranges && !end.is_empty()) {
            Some(end) => (named(locator), named(end)),
            None => {
                let whole = named(locator);
                let dashed = || locator.char_indices().filter(|(_, c)| matches!(c, '-' | '\u{2013}')).find_map(|(at, dash)| {
                    let (first, last) = (named(locator[..at].trim()), named(locator[at + dash.len_utf8()..].trim()));
                    (!first.is_empty() && !last.is_empty()).then_some((first, last))
                });
                if whole.is_empty() && ranges { dashed().unwrap_or_default() } else { (whole.clone(), whole) }
            }
        };
        if first.len() > 1 || last.len() > 1 || first.first().zip(last.first()).is_some_and(|(first, last)| first > last) {
            let mut matches: Vec<usize> = Vec::new();
            for &at in first.iter().chain(&last) {
                if !matches.contains(&sections[at]) {
                    matches.push(sections[at]);
                }
            }
            return Provisions::Ambiguous(matches);
        }
        let (Some(&first), Some(&last)) = (first.first(), last.first()) else {
            return Provisions::NotFound;
        };
        if first == last {
            return Provisions::Found { order: sections, first, last };
        }
        // A range's provisions are the outermost from its first through its last, each with what it holds.
        let spanned: HashSet<&str> = sections[first..=last].iter().map(|&index| document.nodes[index].id.as_str()).collect();
        let span: Vec<usize> = sections[first..=last].iter().copied()
            .filter(|&index| document.nodes[index].parent_id.as_deref().is_none_or(|parent| !spanned.contains(parent)))
            .collect();
        let last = span.len() - 1;
        Provisions::Found { order: span, first: 0, last }
    }
}
