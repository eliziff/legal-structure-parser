//! A court opinion's structure as a lawyer queries it, read once over all its stored versions: its pages by
//! reporter, paragraphs, notes and their calls, headings, headnotes and the marks that key them, its parts and front
//! matter, every place a UTF-16 range of `text`.
//!
//! One rendition is read: the markup CourtListener prefers, through the engine, or where a rendition is only a
//! court's extracted text set in <pre>, the plain text's own layout (`text`). The others supply what it lacks: page
//! numbers placed by the words around them, and headings where a rendition marks a paragraph as one.
//!
//! `text` is the reading text: page marks and page furniture are out of it, and a word a page mark split is whole, so a
//! search for a phrase finds it across a page's turn. Each page's range still says where the page turns.

mod checks;
mod clean;
mod headings;
mod matter;
mod notes;
mod pages;
mod paragraphs;
mod readings;
mod text;

pub use checks::{parse_failures, parse_screens, quote_miss, receipt_miss};
pub use pages::{citers_numbering, citers_page};
use clean::*;
use headings::*;
use matter::*;
use notes::*;
use pages::*;
use paragraphs::*;
use readings::*;

use crate::text::ScalarCoordinates;
use crate::{
    analyze_native_markup, hierarchy, javascript_whitespace, trim_javascript_whitespace as trim_js, utf16_len, Derivation, DocumentStructure,
    MarkupElement, NativeMarkupInput, NodeKind, PrimaryRole, ScalarRange, ScalarText, StructureNode,
};
pub use legal_structure_model::opinion::{FrontLine, OpinionCitation, OpinionHeadnote, OpinionReading, PageSeries, Pin, RecordGap};
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

/// Every rendition that is markup, in CourtListener's order of preference.
const MARKUP: [&str; 6] = ["html_with_citations.html", "xml_harvard.xml", "html_columbia.html", "html_lawbox.html", "html_anon_2020.html", "html.html"];

/// What the engine is asked to read: the record as CourtListener stores it, every rendition with the case's own fields.
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct OpinionRecord {
    pub id: String,
    /// `fields.json`'s values CourtListener keeps apart from the text: its type and author, where they are known.
    pub opinion_type: Option<String>,
    pub author: Option<String>,
    pub page_count: Option<u32>,
    /// File name and contents, each rendition stored.
    pub renditions: Vec<(String, String)>,
    /// The case's citations, CourtListener's table of them: each reporter it is printed in.
    pub citations: Vec<OpinionCitation>,
    /// The opinion cluster's text fields: its case's head matter, syllabus, headnotes and summary, which reporters
    /// print over the opinion and CourtListener keeps on the case.
    pub cluster: Vec<(String, String)>,
}

/// The parser's nodes, read as the parser reads them. A page's label is `page` and its printed label, its numbering
/// `series` (`reporter-1`, CAP's first citation; a Lexis page scheme; `slip`, `ecf`, `sheet`; or a rendition's name
/// where its numbers are not this rendition's), how its mark was found `grammar`; a page a note's own text turns holds
/// `in_note` and says nothing of the opinion's pages. A paragraph's label is `par` and its own number as printed; a
/// quotation set apart is a Quote; its properties hold the label of a list's item that opens it ("(a)", "2."), who speaks
/// it in a transcript ("Q.", "THE COURT"), and `continues` where it carries on the paragraph before it past a note at a
/// page's foot. A heading's title is its `display_value`, its place in the opinion's ladder of enumerators its `level`.
/// A part is a Section whose `display_value` is the writing it is (`opinion`, `dissent`, `concurrence`), its author a
/// property. What else the opinion reads in its text is a Span (`span`).
const ORIGIN: &str = "courtlistener-opinion";

fn node(kind: NodeKind, start: usize, end: usize, source: Derivation) -> StructureNode {
    StructureNode::new(String::new(), kind, ScalarRange { start, end }, ORIGIN, source, None)
}

/// A node of the markup reader's that the parse takes on as its own: the node itself, its identity and its place in the
/// markup (its tag, start and attributes) kept, as the opinion's `kind` over `range`. The rest of the markup reader's reading
/// of it (its anchor, aliases, parents, role, extents and rendered membership, a paragraph's marker) is not the opinion's:
/// the opinion reads each kind's label, series, grammar and properties by its own rules, and cutting the text moves only
/// what the opinion reads.
fn taken(markup: StructureNode, kind: NodeKind, range: ScalarRange, source: Derivation) -> StructureNode {
    let StructureNode { id, markup_tag, markup_start, markup_attributes, .. } = markup;
    StructureNode { id, markup_tag, markup_start, markup_attributes, ..node(kind, range.start, range.end, source) }
}

/// A page whose mark is printed or marked is known natively; one reckoned, carried or inferred is not.
fn page_derivation(source: &str) -> Derivation {
    if matches!(source, "markup" | "printed") { Derivation::Native } else { Derivation::Heuristic }
}

/// A page's reading: its label as printed, its numbering, its printed mark and how the mark was found.
fn as_page(mut page: StructureNode, label: impl std::fmt::Display, series: impl Into<String>, marker: Option<ScalarRange>, source: &str) -> StructureNode {
    page.label = Some(label.to_string());
    page.series = series.into();
    page.marker_range = marker;
    page.grammar = Some(source.to_owned());
    page
}

pub(crate) fn read_page(label: impl std::fmt::Display, series: impl Into<String>, start: usize, end: usize, marker: Option<ScalarRange>, source: &str) -> StructureNode {
    as_page(node(NodeKind::Page, start, end, page_derivation(source)), label, series, marker, source)
}

pub(crate) fn read_paragraph(start: usize, end: usize, number: Option<String>) -> StructureNode {
    let mut paragraph = node(NodeKind::Paragraph, start, end, Derivation::Native);
    paragraph.label = number;
    paragraph
}

pub(crate) fn read_heading(start: usize, end: usize, source: &str) -> StructureNode {
    let mut heading = node(NodeKind::Heading, start, end, Derivation::Native);
    heading.grammar = Some(source.to_owned());
    heading
}

/// A data table, its rows out of the paragraphs.
pub(crate) fn read_table(start: usize, end: usize) -> StructureNode {
    node(NodeKind::Table, start, end, Derivation::Native)
}

/// A part's reading: the writing it is, and who wrote it.
fn as_part(mut part: StructureNode, writing: String, author: Option<String>) -> StructureNode {
    part.display_value = Some(writing);
    if let Some(author) = author {
        part.properties.insert("author".to_owned(), author);
    }
    part
}

pub(crate) fn read_part(writing: String, author: Option<String>, start: usize, end: usize) -> StructureNode {
    as_part(node(NodeKind::Section, start, end, Derivation::Native), writing, author)
}

/// A note's reading: its label as printed, its body's pieces (`ranges`) and the places it is called (`calls`). Its
/// properties hold the labels of a list inside it ("(1)", "A."), one a line, and `called_outside` where its call stands in
/// words of the case this text lacks.
fn as_note(mut note: StructureNode, label: impl std::fmt::Display, ranges: Vec<ScalarRange>, calls: Vec<ScalarRange>) -> StructureNode {
    note.label = Some(label.to_string());
    note.ranges = ranges;
    note.calls = calls;
    note
}

/// A note, a Footnote node.
pub(crate) fn read_note(label: impl std::fmt::Display, start: usize, end: usize, ranges: Vec<ScalarRange>, calls: Vec<ScalarRange>) -> StructureNode {
    as_note(node(NodeKind::Footnote, start, end, Derivation::Native), label, ranges, calls)
}

/// A reading of the opinion's that stands in its text and is no block of it, a Span its `grammar` names: `headnote`,
/// `front` (a line of the front matter), `headnote-mark` (a headnote's number keying a passage), `block-counter` (a counter
/// a source sets on its blocks, its `source`), `unmatched-call` (a note's call whose note the record never reported),
/// `furniture` (a page's running title, number or stamp, where it stood before it left the text), `defined-term`,
/// `quotation`, `quote-mark` (an editor's mark in a quotation, its child) and `instrument` (a document the opinion
/// reproduces). Its role is the structure's where one fits; its properties hold the rest the opinion reads of it. The usual
/// queries serve no span.
fn span(grammar: &str, start: usize, end: usize) -> StructureNode {
    let mut span = node(NodeKind::Span, start, end, Derivation::Native);
    span.grammar = Some(grammar.to_owned());
    span
}

/// Whether a node is the reading `grammar` names.
fn reads(node: &StructureNode, grammar: &str) -> bool {
    node.kind == NodeKind::Span && node.grammar.as_deref() == Some(grammar)
}

/// A headnote, syllabus point or summary paragraph the text holds: its words as read, its kind (`kind`, as
/// `OpinionHeadnote::kind` names them).
fn headnote_node(kind: &str, text: String, range: ScalarRange) -> StructureNode {
    let mut headnote = span("headnote", range.start, range.end);
    headnote.role = Some(PrimaryRole::Headnote);
    headnote.display_value = Some(text);
    headnote.properties.insert("kind".to_owned(), kind.to_owned());
    headnote
}

/// The front matter's roles the structure names, and the opinion's name for each.
const FRONT_ROLES: [(PrimaryRole, &str); 7] = [
    (PrimaryRole::Caption, "caption"), (PrimaryRole::Judges, "judges"), (PrimaryRole::Counsel, "counsel"), (PrimaryRole::Date, "date"),
    (PrimaryRole::DocketNumber, "docket"), (PrimaryRole::Court, "court"), (PrimaryRole::Title, "title"),
];

fn front_name(role: PrimaryRole) -> Option<&'static str> {
    FRONT_ROLES.iter().find(|(known, _)| *known == role).map(|(_, name)| *name)
}

/// A line of the front matter the text holds: its words as read, its role the structure's where it names one, else the
/// opinion's own (`role`, as `FrontLine::role` names them).
fn front_node(role: &str, text: String, range: ScalarRange) -> StructureNode {
    let mut line = span("front", range.start, range.end);
    line.display_value = Some(text);
    name_role(&mut line, role);
    line
}

fn name_role(line: &mut StructureNode, role: &str) {
    line.role = FRONT_ROLES.iter().find(|(_, name)| *name == role).map(|(known, _)| *known);
    if line.role.is_some() {
        line.properties.remove("role");
    } else {
        line.properties.insert("role".to_owned(), role.to_owned());
    }
}

/// A front line's role, as the opinion names it.
pub fn role_of(line: &StructureNode) -> &str {
    line.properties.get("role").map(String::as_str).or_else(|| line.role.and_then(front_name)).unwrap_or("")
}

/// A headnote's number printed in the opinion, "[3]" or "[5, 6]", at the passage it keys.
fn headnote_mark(numbers: &[u32], start: usize, end: usize) -> StructureNode {
    let mut mark = span("headnote-mark", start, end);
    mark.properties.insert("numbers".to_owned(), numbers.iter().map(u32::to_string).collect::<Vec<_>>().join(","));
    mark
}

/// A block counter a source sets, `<div class="num"><span class="num">14</span>`: no number the court printed, but the only
/// one some texts carry, and it may stand on a paragraph, a title or a note. `source`: `marked` in this text,
/// `other-rendition` placed by the words after it, `reckoned` between its neighbours where those words are not in this text.
fn block_counter(label: String, start: usize, source: &str) -> StructureNode {
    let mut counter = span("block-counter", start, start);
    counter.label = Some(label);
    counter.properties.insert("source".to_owned(), source.to_owned());
    counter
}

/// The opinion's readings of one `grammar` that stand in its text, in the text's order.
pub fn spans<'a>(structure: &'a DocumentStructure, grammar: &'a str) -> impl Iterator<Item = &'a StructureNode> {
    structure.nodes.iter().filter(move |node| node.origin_id == ORIGIN && reads(node, grammar))
}

/// Every headnote's kind, number as printed apart from its words, and words: those the text holds, in its order, then those
/// the record holds apart from it.
pub fn headnotes(structure: &DocumentStructure) -> impl Iterator<Item = (&str, Option<&str>, &str)> {
    headnotes_of(spans(structure, "headnote"), reading(structure))
}

fn headnotes_of<'a>(spans: impl Iterator<Item = &'a StructureNode>, reading: &'a OpinionReading) -> impl Iterator<Item = (&'a str, Option<&'a str>, &'a str)> {
    spans.map(|headnote| (headnote.properties.get("kind").map_or("", String::as_str), None, headnote.display_value.as_deref().unwrap_or("")))
        .chain(reading.headnotes.iter().map(|headnote| (headnote.kind.as_str(), headnote.number.as_deref(), headnote.text.as_str())))
}

/// Every front line's role and words: those the text holds, in its order, then those the record holds apart from it.
pub fn front_lines(structure: &DocumentStructure) -> impl Iterator<Item = (&str, &str)> {
    front_lines_of(spans(structure, "front"), reading(structure))
}

fn front_lines_of<'a>(spans: impl Iterator<Item = &'a StructureNode>, reading: &'a OpinionReading) -> impl Iterator<Item = (&'a str, &'a str)> {
    spans.map(|line| (role_of(line), line.display_value.as_deref().unwrap_or("")))
        .chain(reading.front.iter().map(|line| (line.role.as_str(), line.text.as_str())))
}

/// What one pass leaves for later ones beside the structure: the titles other renditions mark, the openings of blocks they set
/// as quotations, each one's paragraph numbers where their words fall in this text, what is yet to leave the text, and the
/// witness a census asked for.
#[derive(Default)]
pub(crate) struct Pending {
    marked_titles: HashSet<String>,
    quoted_elsewhere: HashSet<String>,
    printed_elsewhere: Vec<(bool, Vec<(u32, usize)>)>,
    cuts: Vec<ScalarRange>,
    witness: Option<Witness>,
}

/// What a record's other renditions and texts witness against the reading, for a census over a corpus to tally. No part of
/// the structure: the product never asks for it (`witnessed_opinion_structure`).
#[derive(Debug, Default, serde::Serialize)]
pub struct Witness {
    /// Renditions of this record that hold another opinion's text, and were not read.
    pub other_opinion: Vec<String>,
    /// Other renditions' page marks against these pages, by how the page was found: at its place, a page off, further.
    pub page_witness: BTreeMap<String, [usize; 3]>,
    /// Notes another text of the case prints, against this reading's before they were supplied: held and called, held with
    /// no call, not held.
    pub note_witness: [usize; 3],
    /// Titles another rendition marks as headings: held here as headings, in this text but no heading, not in this text.
    pub heading_witness: [usize; 3],
    /// Paragraphs other renditions open, against ours: opened alike, inside a paragraph of ours, words not in our text; and
    /// paragraphs of ours opening where the others run on.
    pub paragraph_witness: BTreeMap<String, [usize; 4]>,
    /// A few of those run together and those split apart, by their opening words.
    pub paragraphs_run_on: Vec<String>,
    pub paragraphs_split: Vec<String>,
    /// What other renditions offer against the finished reading, by structure: offered, not held.
    pub union_test: BTreeMap<String, [usize; 2]>,
    /// A few of those not held, to read: "page 412", "text the words it opens on".
    pub union_samples: Vec<String>,
    /// The titles other renditions mark that no heading here holds, each as its words.
    pub titles_not_held: Vec<String>,
    /// What each other rendition offers, kept until the reading is final.
    #[serde(skip)]
    elsewhere: Vec<Elsewhere>,
}

fn structure_of(id: &str, plain: &str, markup: &str) -> Option<DocumentStructure> {
    // The paragraph sign read through the wrong code page, byte B6 as Latin-2 or Thai: "ś 56", "2003 VT 34, ถ 7".
    static MISREAD_SIGN: OnceLock<Regex> = OnceLock::new();
    let misread = MISREAD_SIGN.get_or_init(|| Regex::new(r"(?m)(^|[\s>(\[])[śถ](\s?\d)").unwrap());
    // North Dakota's opinions spell the paragraph sign "[&amp;P&amp;12]".
    let markup = misread.replace_all(markup, "${1}¶${2}").replace("&amp;P&amp;", "¶");
    let plain = misread.replace_all(plain, "${1}¶${2}");
    analyze_native_markup(NativeMarkupInput::of(crate::native_markup::DocumentProvider::CourtListener, id, plain.into_owned(), markup)).ok()
}

/// A court's text set in <pre> blocks: markup in name, the extractor's layout in fact.
/// A resource.org page counts its blocks down the margin (`div.num`, `span.num`): that is no number the opinion prints.
/// The block counters a source sets, `<span class="num">14</span>`, each with where it stands; `nodes` are the source's.
fn block_marks(name: &str, structure: &DocumentStructure, nodes: &[StructureNode], coordinates: &ScalarText) -> Vec<(String, usize)> {
    let marks = structure.markup_elements.iter().filter(|element| element.tag == "span" && element.attributes.get("class").is_some_and(|class| class == "num")).filter_map(|element| {
        let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(trim_js)?;
        (!printed.is_empty() && printed.bytes().all(|byte| byte.is_ascii_digit())).then(|| (printed.to_owned(), element.range.start))
    }).collect::<Vec<_>>();
    if marks.is_empty() && name == "html_columbia.html" { bracket_counters(structure, nodes, coordinates) } else { marks }
}

/// Columbia's counter on each block, "[1] OPINION", "[2] Kenneth Joe Rider": a count of its blocks, title and all, that the
/// opinion never printed, and that holds on most of the blocks or is some other thing.
fn bracket_counters(structure: &DocumentStructure, nodes: &[StructureNode], coordinates: &ScalarText) -> Vec<(String, usize)> {
    const LEAST: usize = 4;
    static COUNTER: OnceLock<Regex> = OnceLock::new();
    let counter = COUNTER.get_or_init(|| Regex::new(r"^\[(\d{1,3})\]\s+\S").unwrap());
    let blocks = nodes.iter().filter(|node| matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Heading)).filter_map(|node| {
        let range = structure.query_range(node);
        Some((coordinates.slice_utf16(range.start..range.end).map(trim_js)?, range.start))
    }).collect::<Vec<_>>();
    let mut due = 1;
    let mut found = Vec::new();
    let mut span = (usize::MAX, 0);
    for (index, (printed, start)) in blocks.iter().enumerate() {
        if counter.captures(printed).is_some_and(|capture| capture[1].parse::<u32>().ok() == Some(due)) {
            found.push((due.to_string(), *start));
            due += 1;
            span = (span.0.min(index), index);
        }
    }
    // Counsel and the court's name stand ahead of the first counter, the notes past the last.
    if found.len() >= LEAST && found.len() * 3 > span.1 - span.0 { found } else { Vec::new() }
}

/// The paragraph numbers a rendition prints, read as ours are read, each with where it stands, and whether its marks say they
/// are paragraphs'. Bare numbers must hold on half the blocks: a headnote's "[3]" may open a block as well.
fn printed_numbers(structure: &DocumentStructure, coordinates: &ScalarText) -> (bool, Vec<(u32, usize)>) {
    const LEAST: usize = 3;
    let nodes = structure.nodes.iter().filter(|node| matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose)).collect::<Vec<_>>();
    let openings = nodes.iter().filter_map(|node| { let range = structure.query_range(node); Some((range.start, coordinates.slice_utf16(range.start..range.end).map(trim_js)?)) }).collect::<Vec<_>>();
    // A named mark outranks a list beside it: "¶ 1 ..." over its "1)", "2)", "3)".
    let of_name = text::own_numbers(openings.iter().enumerate().filter(|(_, (_, opening))| text::says_paragraph(opening)).map(|(index, (_, opening))| (index, *opening)), LEAST);
    let read = if of_name.len() >= LEAST { of_name } else { text::own_numbers(openings.iter().enumerate().map(|(index, (_, opening))| (index, *opening)), LEAST) };
    let named = read.keys().any(|index| text::says_paragraph(openings[*index].1));
    let mut found = if named || read.len() * 2 >= openings.len() { read.into_iter().map(|(index, number)| (openings[index].0, number)).collect::<BTreeMap<_, _>>() } else { BTreeMap::new() };
    found.extend(nodes.iter().filter_map(|node| Some((structure.query_range(node).start, node.label.as_deref()?.parse::<u32>().ok()?))));
    (named, found.into_iter().map(|(start, number)| (number, start)).collect())
}

fn counts_its_blocks(structure: &DocumentStructure) -> bool {
    structure.markup_elements.iter().any(|element| element.tag == "div" && element.attributes.get("class").is_some_and(|class| class == "num"))
}

/// Whether two texts are one opinion: a record's rendition can hold another case altogether, and then it witnesses nothing.
/// Scanners break runs of words but leave a good many whole, and the shorter text may be an excerpt of the longer.
fn same_opinion(ours: &str, theirs: &str) -> bool {
    const SHARE: usize = 20;
    let (ours, theirs) = (places(ours), places(theirs));
    let held = ours.iter().collect::<HashSet<_>>();
    theirs.is_empty() || ours.is_empty() || theirs.iter().filter(|place| held.contains(place)).count() * SHARE >= ours.len().min(theirs.len())
}

/// The stretches of the text no paragraph, heading or note among `nodes`, nor headnote or line of the front, holds.
pub(crate) fn unheld_stretches<'a>(structure: &DocumentStructure, nodes: impl Iterator<Item = &'a StructureNode>) -> Vec<(usize, usize)> {
    let mut held = nodes.flat_map(|node| match node.kind {
            NodeKind::Paragraph | NodeKind::Quote | NodeKind::Heading => vec![(node.range.start, node.range.end)],
            NodeKind::Footnote => node.ranges.iter().map(|range| (range.start, range.end)).collect(),
            NodeKind::Span if reads(node, "headnote") || reads(node, "front") => vec![(node.range.start, node.range.end)],
            _ => Vec::new(),
        })
        .collect::<Vec<_>>();
    held.sort_unstable();
    let whole = utf16_len(&structure.text);
    let mut cursor = 0;
    let mut stretches = Vec::new();
    for (start, end) in held.into_iter().chain([(whole, whole)]) {
        if start > cursor {
            stretches.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    stretches
}

/// What is left in the text of two faults of the source: letters in the shifted font, and runs of a pleading's line numbers
/// (five or more lines in a row opening on numbers that climb by one).
pub fn source_faults_left(structure: &DocumentStructure) -> (usize, usize) {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    let lead = LEAD.get_or_init(|| Regex::new(r"^\s{0,8}(\d{1,2})(?:\s|$)").unwrap());
    const RUN: usize = 5;
    let (mut margins, mut run, mut last) = (0, 0, 0);
    for line in structure.text.lines().filter(|line| !line.trim().is_empty()) {
        let number = lead.captures(line).and_then(|capture| capture[1].parse::<u32>().ok());
        run = if number.is_some_and(|number| number == last + 1) { run + 1 } else { 1 };
        if run == RUN { margins += RUN; } else if run > RUN { margins += 1; }
        last = number.unwrap_or(0);
    }
    (structure.text.matches('\u{3}').count(), margins)
}

/// Lines inside notes that open on a paragraph's own mark: a note has run on through the opinion.
pub fn marks_in_notes(structure: &DocumentStructure) -> usize {
    let coordinates = ScalarText::new(&structure.text);
    nodes(structure, NodeKind::Footnote).flat_map(|note| note.ranges.iter()).filter_map(|range| coordinates.slice_utf16(range.start..range.end))
        .map(|body| body.lines().skip(1).filter(|line| text::says_paragraph(line.trim_start())).count()).sum()
}

/// How much of the text (spaces aside) nothing holds, and how the longest such stretch opens.
pub fn unheld_text(structure: &DocumentStructure) -> (usize, String) {
    let coordinates = ScalarText::new(&structure.text);
    let (mut total, mut longest) = (0, (0, String::new()));
    for (start, end) in unheld_stretches(structure, structure.nodes.iter().filter(|node| node.origin_id == ORIGIN)) {
        let printed = coordinates.slice_utf16(start..end).map_or("", trim_js);
        let size = printed.chars().filter(|c| !c.is_whitespace()).count();
        total += size;
        if size > longest.0 {
            longest = (size, printed.chars().take(80).collect());
        }
    }
    (total, longest.1)
}
/// What the parse works on while it reads: a list each of its pages, its paragraphs (quotations set apart among them), its
/// notes, headings, parts, data tables and spans, its readings that have no place in the text, and the index of the text's
/// coordinates the passes read. The structure holds the rest, and takes these when the parse ends.
pub(crate) struct Kinds {
    pub(crate) pages: Vec<StructureNode>,
    pub(crate) paragraphs: Vec<StructureNode>,
    pub(crate) notes: Vec<StructureNode>,
    pub(crate) headings: Vec<StructureNode>,
    pub(crate) parts: Vec<StructureNode>,
    pub(crate) tables: Vec<StructureNode>,
    pub(crate) spans: Vec<StructureNode>,
    pub(crate) opinion: OpinionReading,
    /// The structure's text indexed once: built where the text is fixed, again where the parse changes it.
    pub(crate) coordinates: ScalarCoordinates,
}

impl Kinds {
    /// Nothing read yet of `text`, its index built.
    pub(crate) fn of(text: &str) -> Self {
        Self { pages: Vec::new(), paragraphs: Vec::new(), notes: Vec::new(), headings: Vec::new(), parts: Vec::new(), tables: Vec::new(), spans: Vec::new(),
            opinion: OpinionReading::default(), coordinates: ScalarCoordinates::new(text) }
    }

    fn all_mut(&mut self) -> impl Iterator<Item = &mut StructureNode> {
        self.pages.iter_mut().chain(&mut self.paragraphs).chain(&mut self.notes).chain(&mut self.headings).chain(&mut self.parts).chain(&mut self.tables).chain(&mut self.spans)
    }

    /// The structure takes what the parse read.
    fn finish(self, structure: &mut DocumentStructure) {
        structure.opinion = Some(Box::new(self.opinion));
        structure.nodes.extend(self.pages.into_iter().chain(self.paragraphs).chain(self.notes).chain(self.headings).chain(self.parts).chain(self.tables).chain(self.spans));
    }

    /// A headnote: a span where the text holds it, the record's reading where its words stand apart.
    fn headnote(&mut self, kind: &str, text: String, range: Option<ScalarRange>) {
        match range {
            Some(range) => self.spans.push(headnote_node(kind, text, range)),
            None => self.opinion.headnotes.push(OpinionHeadnote { kind: kind.to_owned(), number: None, text }),
        }
    }

    /// A line of the front matter: a span where the text holds it, the record's reading where its words stand apart.
    fn front(&mut self, role: &str, text: String, range: Option<ScalarRange>) {
        match range {
            Some(range) => self.spans.push(front_node(role, text, range)),
            None => self.opinion.front.push(FrontLine { role: role.to_owned(), text }),
        }
    }

    fn headnotes(&self) -> impl Iterator<Item = (&str, Option<&str>, &str)> {
        headnotes_of(self.spans_of("headnote"), &self.opinion)
    }

    fn front_lines(&self) -> impl Iterator<Item = (&str, &str)> {
        front_lines_of(self.spans_of("front"), &self.opinion)
    }

    fn spans_of<'a>(&'a self, grammar: &'a str) -> impl Iterator<Item = &'a StructureNode> {
        self.spans.iter().filter(move |span| reads(span, grammar))
    }
}

/// A page mark of the opinion's own run of pages: neither one a note carries on to its next page nor a stray mark.
pub fn in_flow(page: &StructureNode) -> bool {
    !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray")
}

/// The structure's nodes of a kind the opinion parse wrote, in the text's order.
pub fn nodes(structure: &DocumentStructure, kind: NodeKind) -> impl Iterator<Item = &StructureNode> {
    structure.nodes.iter().filter(move |node| node.kind == kind && node.origin_id == ORIGIN)
}

/// The opinion's paragraphs, quotations set apart among them, in the text's order.
pub fn paragraphs(structure: &DocumentStructure) -> impl Iterator<Item = &StructureNode> {
    structure.nodes.iter().filter(|node| node.origin_id == ORIGIN && matches!(node.kind, NodeKind::Paragraph | NodeKind::Quote))
}

/// The opinion's readings that have no place in its text.
pub fn reading(structure: &DocumentStructure) -> &OpinionReading {
    static NONE: OnceLock<OpinionReading> = OnceLock::new();
    structure.opinion.as_deref().unwrap_or_else(|| NONE.get_or_init(OpinionReading::default))
}

/// Where a passage falls in one numbering. Words in a note stand where the note is called; words past an estimated mark, or
/// in a numbering that has only its first page, stand on no page that can be named.
pub fn pin_at(structure: &DocumentStructure, series: &str, start: usize, end: usize) -> Option<Pin> {
    let middle = start + (end.saturating_sub(start)) / 2;
    if let Some(note) = nodes(structure, NodeKind::Footnote).find(|note| note.ranges.iter().any(|range| range.start <= middle && middle < range.end)) {
        // A note that runs on to the next page carries that page's mark: words past it are on that page, not the call's.
        let run_on = nodes(structure, NodeKind::Page).filter(|page| page.properties.contains_key("in_note") && page.series == series && page.grammar.as_deref() != Some("stray")
            && page.range.start <= middle && note.ranges.iter().any(|range| range.start <= page.range.start && page.range.start < range.end)).max_by_key(|page| page.range.start);
        // A call on a page's last line names no page: its note may be set on the next.
        if note.properties.contains_key("call_at_foot") {
            return None;
        }
        let call = note.calls.first().map_or(note.range.start, |call| call.start);
        let pin = pin_of(structure, series, call, call + 1);
        // A note whose call names no page in this numbering: where its words happen to stand in the text says nothing.
        if pin.is_none() && !note.calls.is_empty() {
            return None;
        }
        if let Some(pin) = pin {
            return Some(run_on.map_or(pin.clone(), |page| Pin { page: page.label.as_deref().unwrap_or("").to_owned(), through: None, ..pin }));
        }
    }
    pin_of(structure, series, start, end)
}

/// Where a stretch of the text falls in one numbering, by the page marks alone.
fn pin_of(structure: &DocumentStructure, series: &str, start: usize, end: usize) -> Option<Pin> {
    pages::pins_of(&pages::pin_table(nodes(structure, NodeKind::Page), &reading(structure).page_series), start, end).into_iter().find(|pin| pin.series == series)
}

/// The paragraph numbers a passage falls under, first and last. Text between numbers, a block quotation's lead-out, is the
/// paragraph above it.
pub fn numbers_at(structure: &DocumentStructure, start: usize, end: usize) -> Option<(String, String)> {
    let paragraphs = paragraphs(structure).collect::<Vec<_>>();
    let under = |place: usize| paragraphs[..paragraphs.partition_point(|paragraph| paragraph.range.start <= place)].iter().rev()
        .find_map(|paragraph| paragraph.label.is_some().then(|| paragraph.label.as_deref().unwrap_or("").trim_matches(['(', ')', '[', ']', '{', '}', '.']).to_owned()));
    Some((under(start)?, under(end)?))
}

fn set_as_text(markup: &str) -> bool {
    markup.contains("<pre") && !markup.contains("<p>") && !markup.contains("<p ") && !markup.contains("<div class=\"num\"")
}

/// A record read whole, as the structure the product serves: the parts, notes, pages, paragraphs and headings are its nodes,
/// and the readings that stand in its text its spans, in the text's order; the opinion's readings with no place in the text
/// are its `opinion`.
pub fn opinion_structure(record: &OpinionRecord, url: Option<String>) -> DocumentStructure {
    read_record(record, url, None).0
}

/// A record read whole, and what its other renditions and texts witness against the reading.
pub fn witnessed_opinion_structure(record: &OpinionRecord) -> (DocumentStructure, Witness) {
    let (structure, witness) = read_record(record, None, Some(Witness::default()));
    (structure, witness.unwrap_or_default())
}

fn read_record(record: &OpinionRecord, url: Option<String>, witness: Option<Witness>) -> (DocumentStructure, Option<Witness>) {
    let rendition = |name: &str| record.renditions.iter().find(|(file, _)| file == name).map(|(_, text)| text.as_str());
    let plain = rendition("plain_text.txt").unwrap_or("");
    let chosen = MARKUP.iter().copied().find(|name| rendition(name).is_some_and(|markup| !markup.trim().is_empty() && !set_as_text(markup)));
    let mut pending = Pending { witness, ..Pending::default() };
    let (mut structure, mut kinds) = match chosen.and_then(|name| Some((name, structure_of(&record.id, plain, rendition(name)?)?))) {
        Some((name, markup)) => {
            let (mut structure, mut kinds) = from_structure(record, name, markup, &mut pending);
            leaves_to_pages(&mut kinds, record);
            for other in MARKUP.iter().copied().filter(|other| *other != name) {
                if let Some(text) = rendition(other).filter(|markup| !set_as_text(markup)) {
                    // The same text, by CourtListener's own derivation.
                    if name == "html_with_citations.html" && other == "xml_harvard.xml" {
                        continue;
                    }
                    if let Some(other_structure) = structure_of(&record.id, plain, text) {
                        if !same_opinion(&structure.text, other_structure.query_text()) {
                            if let Some(witness) = pending.witness.as_mut() {
                                witness.other_opinion.push(other.to_owned());
                            }
                            continue;
                        }
                        let theirs = ScalarText::new(other_structure.query_text());
                        supply_from(&mut structure, &mut kinds, &mut pending, other, &other_structure, &theirs);
                        supply_notes(&mut structure, &mut kinds, &mut pending, &other_structure, &theirs);
                        supply_head(&mut kinds, &other_structure, &theirs);
                    }
                }
            }
            (structure, kinds)
        }
        None => {
            let (name, text) = if plain.trim().is_empty() {
                let markup = MARKUP.iter().copied().find_map(|name| rendition(name).map(|markup| (name, markup)));
                let text = markup.and_then(|(_, markup)| structure_of(&record.id, "", markup)).map(|structure| text::unshifted(structure.query_text()));
                (markup.map_or("", |pair| pair.0), text.unwrap_or_default())
            } else {
                ("plain_text.txt", text::unshifted(plain))
            };
            from_text(record, name, text, &mut pending)
        }
    };
    if kinds.opinion.rendition != "plain_text.txt" && !plain.trim().is_empty() {
        supply_plain_pages(&mut structure, &mut kinds, &text::unshifted(plain), record.page_count);
    }
    complete(&mut structure, &mut kinds, &mut pending, record, plain);
    kinds.finish(&mut structure);
    // Each node its id by kind and place, all of them in the text's order.
    let mut counts = HashMap::<NodeKind, usize>::new();
    for node in structure.nodes.iter_mut().filter(|node| node.id.is_empty()) {
        let count = counts.entry(node.kind).or_default();
        *count += 1;
        node.id = format!("opinion-{}-{count}", node.kind.name());
    }
    // At one start, what stands at that point before what runs on from it (a page reckoned ahead of the text, then the page
    // its first mark opens), then the longer before the shorter.
    structure.nodes.sort_by_key(|node| (node.range.start, node.range.end != node.range.start, std::cmp::Reverse(node.range.end)));
    heading_ladder(&mut structure);
    structure.url = url;
    // The text is final only now: page marks, furniture and calls are out of it, and other texts' notes in.
    structure.text_sha256 = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(structure.text.as_bytes()));
    legal_structure_model::write_levels(&mut structure.nodes).expect("an opinion's parts hold its paragraphs, a heading the titles under it, a quotation its marks, and nothing holds a part");
    structure.refresh_revision().expect("Typed document structure is serializable");
    (structure, pending.witness)
}

/// The structure the parse writes into (the reading text, the record's id, the rendition read), beside the nodes a reader
/// read, each in its kind's list.
fn opinion_document(record: &OpinionRecord, rendition: &str, text: String, mut kinds: Kinds) -> (DocumentStructure, Kinds) {
    let mut structure = DocumentStructure::new(record.id.clone(), "courtlistener", text, ORIGIN);
    structure.doc_type = Some("cases".to_owned());
    structure.profile = Some(crate::DetectionProfile::CaseLossy);
    kinds.opinion.rendition = rendition.to_owned();
    (structure, kinds)
}

/// Where another rendition prints a page at a place in this text, what does this structure say there? At that page's own start in any
/// of its numberings, within `tolerance`, it agrees; otherwise the nearest numbering answers, by how many pages it is off.
fn witness_pages(pages: &[StructureNode], witness: &mut Witness, marks: &[(u32, usize)], tolerance: usize) {
    let mut numberings = HashMap::<&str, Vec<(usize, u32, &str)>>::new();
    for page in pages.iter().filter(|page| in_flow(page)) {
        if let Some(number) = page_number(page.label.as_deref().unwrap_or("")) {
            numberings.entry(page.series.as_str()).or_default().push((page.range.start, number, page.grammar.as_deref().unwrap_or("")));
        }
    }
    for run in numberings.values_mut() {
        run.sort_unstable();
    }
    let mut verdicts = Vec::new();
    for &(number, place) in marks {
        let held = numberings.values().flatten().find(|page| page.1 == number && page.0.abs_diff(place) <= tolerance);
        let verdict = match held {
            Some(page) => Some((page.2, 0)),
            None => numberings.values().filter_map(|run| run[..run.partition_point(|page| page.0 <= place)].last()).min_by_key(|page| page.1.abs_diff(number))
                .map(|page| (page.2, page.1.abs_diff(number).clamp(1, 2) as usize)),
        };
        verdicts.extend(verdict);
    }
    for (source, verdict) in verdicts {
        witness.page_witness.entry(source.to_owned()).or_default()[verdict] += 1;
    }
}

/// A title another rendition marks, without a page mark at its head or a note's call at its tail.
fn bare_title(title: &str) -> String {
    let spaced = title.split(' ').all(|word| word.chars().count() == 1);
    let title = if spaced { title.replace(' ', "") } else { title.to_owned() };
    let digits = |word: &str| word.bytes().all(|byte| byte.is_ascii_digit());
    let words = title.split(' ').collect::<Vec<_>>();
    let from = usize::from(words.len() > 1 && digits(words[0]) && words[0].len() >= 3);
    let to = words.len() - usize::from(words.len() > from + 1 && digits(words[words.len() - 1]));
    words[from..to].join(" ")
}

/// Whether a marked title is a section's: not a case's name, a list of notes, or a name in a table of cases.
fn section_title(title: &str) -> bool {
    !title.contains(" v ") && !title.contains(" vs ") && !title.starts_with("in re ") && !title.starts_with("in the matter of ") && !notes_title(title)
}

/// A paragraph of ours that opens on a title another rendition marks has that title at its head, or is that title.
fn take_marked_titles(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending) {
    if pending.marked_titles.is_empty() {
        return;
    }
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let key = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>().join(" ");
    let ours = kinds.headings.iter().filter_map(|heading| coordinates.slice_utf16(heading.range.start..heading.range.end)).map(|text| bare_title(&key(text))).collect::<HashSet<_>>();
    let mut wanted = HashMap::<String, Vec<Vec<String>>>::new();
    for title in pending.marked_titles.iter().filter(|title| section_title(title)).map(|title| bare_title(title)).filter(|title| title.len() > 3 && !ours.contains(title)) {
        let words = title.split(' ').map(str::to_owned).collect::<Vec<_>>();
        wanted.entry(words[0].clone()).or_default().push(words);
    }
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(kinds.paragraphs.len());
    for mut paragraph in std::mem::take(&mut kinds.paragraphs) {
        let raw = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).unwrap_or("");
        let words = word_starts(raw);
        let title = words.first().and_then(|first| wanted.get(&first.0)).and_then(|titles| {
            titles.iter().filter(|title| title.len() <= words.len() && title.iter().zip(&words).all(|(word, ours)| *word == ours.0)).max_by_key(|title| title.len())
        });
        let Some(title) = title.filter(|_| !(paragraph.kind == NodeKind::Quote) && paragraph.label.is_none()) else {
            kept.push(paragraph);
            continue;
        };
        let last = &words[title.len() - 1];
        let end = paragraph.range.start + last.1 + utf16_len(&last.0);
        taken.push(read_heading(paragraph.range.start + words[0].1, end, "other-rendition"));
        if words.len() > title.len() {
            paragraph.range.start = end;
            kept.push(paragraph);
        }
    }
    kinds.paragraphs = kept;
    kinds.headings.append(&mut taken);
}

/// The titles other renditions mark, against the headings this reading came to. A case's name and a list of notes are no section's
/// title. A title compares by its words alone, without a page mark at its head or a note's call at its tail, and may stand in two
/// headings here, "IV." over "Conclusion". Of those not held, the words that stand in this text are titles missed.
fn witness_headings(structure: &DocumentStructure, kinds: &Kinds, pending: &mut Pending) {
    let Some(witness) = pending.witness.as_mut() else { return };
    let key = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>().join(" ");
    // A title's paragraph or outline number stays off it here: a rendition may print the mark inside what it marks.
    let unnumbered = |title: String| title.split(' ').skip_while(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit())).collect::<Vec<_>>().join(" ");
    let keys = kinds.headings.iter().map(|heading| key(&heading.display_value.as_deref().unwrap_or(""))).collect::<Vec<_>>();
    let mut ours = keys.iter().map(|title| unnumbered(bare_title(title))).collect::<HashSet<_>>();
    ours.extend(keys.windows(2).map(|pair| unnumbered(bare_title(&format!("{} {}", pair[0], pair[1])))));
    let wanted = pending.marked_titles.iter().filter(|title| section_title(title)).map(|title| unnumbered(bare_title(title))).collect::<HashSet<_>>();
    let mut unheld = wanted.iter().filter(|title| !ours.contains(*title)).collect::<Vec<_>>();
    // In the order of their words, so that one record reads alike on every run.
    unheld.sort_unstable();
    let text = if unheld.is_empty() { String::new() } else { format!(" {} ", key(&structure.text)) };
    witness.titles_not_held = unheld.iter().filter(|title| text.contains(&format!(" {title} "))).map(|title| (*title).clone()).collect();
    witness.heading_witness = [wanted.len() - unheld.len(), witness.titles_not_held.len(), unheld.len() - witness.titles_not_held.len()];
}

/// Marks found by their words keep their order: one out of it found the words somewhere else, a caption restated.
fn in_their_order(places: Vec<Option<usize>>) -> Vec<Option<usize>> {
    let found = places.iter().enumerate().filter_map(|(at, place)| Some((at, u32::try_from((*place)?).ok()?))).collect::<Vec<_>>();
    let kept = text::climbing_run(&found).into_iter().map(|mark| mark.0).collect::<HashSet<_>>();
    places.into_iter().enumerate().map(|(at, place)| place.filter(|_| kept.contains(&at))).collect()
}

/// Marks whose words this text lacks stand by their numbers: ahead of the text if nothing comes before them, as head matter's
/// do, else reckoned by number between the pages either side. A mark that joins no run of the numbering, like the "*Page 1"
/// some renditions close on, is held as a stray: printed, and no page of the run.
fn place_by_number(structure: &mut DocumentStructure, kinds: &mut Kinds, series: &str, labels: Vec<String>) {
    let held = |pages: &[StructureNode]| pages.iter().filter(|page| page.series == series).filter_map(|page| Some((page_number(page.label.as_deref().unwrap_or(""))?, page.range.start))).collect::<Vec<_>>();
    let mut known = held(&kinds.pages).into_iter().map(|page| page.0).collect::<HashSet<_>>();
    let open = known.is_empty();
    let mut numbered = labels.into_iter().filter_map(|label| Some((page_number(&label)?, label))).collect::<Vec<_>>();
    numbered.sort_by_key(|mark| mark.0);
    // Up the numbers, then down: a run of marks joins through its neighbours.
    let mut joined = HashSet::new();
    for mark in numbered.iter().chain(numbered.iter().rev()) {
        if open || known.contains(&(mark.0 + 1)) || known.contains(&mark.0.saturating_sub(1)) {
            known.insert(mark.0);
            joined.insert(mark.0);
        }
    }
    let held_pages = held(&kinds.pages);
    let text_end = utf16_len(&structure.text);
    let word_places = word_starts(&structure.text).into_iter().map(|word| word.1).collect::<Vec<_>>();
    for (number, label) in numbered {
        let past_the_last = !held_pages.is_empty() && held_pages.iter().all(|page| page.0 < number);
        if !joined.contains(&number) || past_the_last {
            kinds.pages.push(read_page(label, series.to_owned(), text_end, text_end, None, "stray"));
            continue;
        }
        let lowest = joined.iter().copied().min().unwrap_or(number);
        let highest = joined.iter().copied().max().unwrap_or(number);
        let below = held_pages.iter().filter(|page| page.0 < number).max_by_key(|page| page.0).copied().or(held_pages.is_empty().then_some((lowest, 0)));
        let above = held_pages.iter().filter(|page| page.0 > number).min_by_key(|page| page.0);
        let start = below.map_or(0, |low| {
            let (high_number, high_start) = above.map_or((highest + 1, text_end), |high| *high);
            let reckoned = low.1 + high_start.saturating_sub(low.1) * (number - low.0) as usize / (high_number - low.0) as usize;
            word_places.get(word_places.partition_point(|place| *place < reckoned)).copied().unwrap_or(reckoned)
        });
        kinds.pages.push(read_page(label, series.to_owned(), start, 0, None, "reckoned"));
    }
}

/// The numbering a plain text prints on its pages, where the markup chosen has lost it: each page placed in our text by the
/// words it opens on.
fn supply_plain_pages(structure: &mut DocumentStructure, kinds: &mut Kinds, plain: &str, page_count: Option<u32>) {
    const SHINGLE: usize = 5;
    let read = text::read(plain, page_count).kinds;
    let plain_pages = read.pages.into_iter().filter(|page| page.series != "sheet").collect::<Vec<_>>();
    if plain_pages.is_empty() || plain_pages.iter().any(|page| kinds.pages.iter().any(|ours| ours.series == page.series)) {
        return;
    }
    let shingle = |window: &[(String, usize)]| window.iter().map(|word| word.0.as_str()).collect::<Vec<_>>().join(" ");
    let words = word_starts(&structure.text);
    let mut index = HashMap::<String, Option<usize>>::new();
    for (at, window) in words.windows(SHINGLE).enumerate() {
        index.entry(shingle(window)).and_modify(|place| *place = None).or_insert(Some(at));
    }
    let coordinates = ScalarText::with_coordinates(plain, &read.coordinates);
    let places = in_their_order(plain_pages.iter().map(|page| {
        let opening = (0..4).find_map(|short| coordinates.slice_utf16(page.range.start..(page.range.start + 400 - short).min(page.range.end))).unwrap_or("");
        let theirs = word_starts(opening);
        theirs.windows(SHINGLE).enumerate().take(12).find_map(|(ahead, window)| index.get(&shingle(window)).copied().flatten()?.checked_sub(ahead))
    }).collect());
    let mut unplaced = HashMap::<String, Vec<String>>::new();
    for (page, place) in plain_pages.into_iter().zip(places) {
        match place {
            Some(place) => kinds.pages.push(read_page(page.label.as_deref().unwrap_or(""), page.series.clone(), words[place].1, 0, None, "other-rendition")),
            None => unplaced.entry(page.series.clone()).or_default().push(page.label.as_deref().unwrap_or("").to_owned()),
        }
    }
    for (series, labels) in unplaced {
        place_by_number(structure, kinds, &series, labels);
    }
}

/// What every reading of a record goes through alike.
fn complete(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending, record: &OpinionRecord, plain: &str) {
    let rendition = |name: &str| record.renditions.iter().find(|(file, _)| file == name).map(|(_, text)| text.as_str());
    printed_page_marks(structure, kinds, &record.citations);
    flat_notes(structure, kinds);
    from_cluster(structure, kinds, pending, record);
    split_note_lists(structure, kinds);
    answer_calls(structure, kinds);
    excise(structure, kinds, std::mem::take(&mut pending.cuts), &mut pending.printed_elsewhere);
    kinds.opinion.citations = record.citations.clone();
    if kinds.headnotes().next().is_none() {
        let found = record.renditions.iter().filter(|(_, text)| names_a_digest(text)).find_map(|(name, text)| {
            let read = if name.ends_with(".txt") { text::unshifted(text) } else { structure_of(&record.id, "", text)?.query_text().to_owned() };
            titled_digest(&read)
        });
        if let Some((kind, body)) = found {
            kinds.headnote(kind, body, None);
        }
    }
    // Lawbox alone may hold the case's own citation.
    if !kinds.front_lines().any(|(role, _)| role == "citation") {
        static OWN: OnceLock<Regex> = OnceLock::new();
        let own = OWN.get_or_init(|| Regex::new(r"<center><b>([^<]{5,60}\(\d{4}\))</b></center>").unwrap());
        if let Some(capture) = rendition("html_lawbox.html").and_then(|markup| own.captures(markup)) {
            kinds.front("citation", squeezed(&capture[1]), None);
        }
    }
    kinds.opinion.headnotes.extend(topic_index(plain));
    kinds.opinion.headnotes.extend(rendition("html_columbia.html").map(embedded_topics).unwrap_or_default());
    if kinds.parts.is_empty() {
        kinds.parts.push(read_part(record.opinion_type.clone().unwrap_or_else(|| "opinion".to_owned()), record.author.clone(), 0, utf16_len(&structure.text)));
    }
    finish(structure, kinds, pending, record);
}

/// A number and nothing else, as a note's label stands when its note has been read: "[6]", "24".
fn bare_label(words: &str) -> bool {
    words.len() <= 8 && words.chars().any(|c| c.is_ascii_digit()) && words.chars().all(|c| c.is_ascii_digit() || matches!(c, '[' | ']' | '(' | ')' | '.'))
}

fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Points run together in one block, "1. Courts must ... 2. A motion ...": split where the numbers climb by ones after a finished
/// sentence. One point stands alone only with a second to follow it.
fn split_points(paragraph: String) -> Vec<String> {
    static POINT: OnceLock<Regex> = OnceLock::new();
    let point = POINT.get_or_init(|| Regex::new(r#"(?:^|[.;:!?”")]\s+)(\d{1,3})\.\s+\p{Lu}"#).unwrap());
    let mut cuts = Vec::new();
    let mut due = None;
    for capture in point.captures_iter(&paragraph) {
        let (Some(number), Some(digits)) = (capture[1].parse::<u32>().ok(), capture.get(1)) else { continue };
        if due.is_none_or(|due| number == due) {
            cuts.push(digits.start());
            due = Some(number + 1);
        }
    }
    if cuts.len() < 2 {
        return vec![paragraph];
    }
    let mut pieces = Vec::with_capacity(cuts.len() + 1);
    if cuts[0] > 0 { pieces.push(paragraph[..cuts[0]].trim().to_owned()); }
    for (index, &from) in cuts.iter().enumerate() {
        pieces.push(paragraph[from..cuts.get(index + 1).copied().unwrap_or(paragraph.len())].trim().to_owned());
    }
    pieces
}

/// A cluster field's paragraphs, whether it is markup or text.
fn field_paragraphs(id: &str, value: &str) -> Vec<String> {
    whole_paragraphs(id, value).into_iter().flat_map(split_points).collect()
}

fn whole_paragraphs(id: &str, value: &str) -> Vec<String> {
    if value.contains('<') {
        if let Some(structure) = structure_of(id, "", value) {
            let coordinates = ScalarText::new(structure.query_text());
            let paragraphs = structure.nodes.iter().filter(|node| matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose))
                .filter_map(|node| {
                    let range = structure.query_range(node);
                    coordinates.slice_utf16(range.start..range.end).map(squeezed)
                }).filter(|text| !text.is_empty()).collect::<Vec<_>>();
            if !paragraphs.is_empty() {
                return paragraphs;
            }
            let whole = squeezed(structure.query_text());
            return if whole.is_empty() { Vec::new() } else { vec![whole] };
        }
    }
    value.split("\n\n").map(squeezed).filter(|text| !text.is_empty()).collect()
}

/// What the case's record says over its opinions: the headnotes and syllabus, one entry a point, and the front matter;
/// the head matter gives what the record's own fields do not.
fn from_cluster(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending, record: &OpinionRecord) {
    let field = |name: &str| record.cluster.iter().find(|(field, _)| field == name).map(|(_, value)| value.as_str());
    for kind in ["syllabus", "headnotes", "summary", "arguments", "history", "posture", "procedural_history", "cross_reference", "correction"] {
        if let Some(value) = field(kind) {
            for text in field_paragraphs(&record.id, value) {
                kinds.headnote(kind, text, None);
            }
        }
    }
    for (name, role) in [("case_name", "case_name"), ("case_name_full", "case_name_full"), ("case_name_short", "case_name_short"), ("date_filed", "date_filed"),
        ("judges", "judges"), ("attorneys", "counsel"), ("disposition", "disposition"), ("other_dates", "other_dates"), ("nature_of_suit", "nature_of_suit"),
        ("precedential_status", "publication"), ("court", "court"), ("docket_number", "docket"), ("appeal_from_str", "lower_court"), ("assigned_to_str", "assigned_to"),
        ("referred_to_str", "referred_to"), ("panel_str", "panel"), ("cause", "cause"), ("date_argued", "date_argued"), ("date_reargued", "date_reargued"),
        ("date_reargument_denied", "date_reargument_denied"), ("date_cert_granted", "date_cert_granted"), ("date_cert_denied", "date_cert_denied"),
        ("date_terminated", "date_terminated"), ("scdb_votes_majority", "votes_majority"), ("scdb_votes_minority", "votes_minority"),
        ("citation_count", "cited_by")] {
        if let Some(value) = field(name) {
            kinds.front(role, squeezed(value), None);
        }
    }
    let Some(matter) = field("headmatter").and_then(|text| structure_of(&record.id, "", text)) else { return };
    let coordinates = ScalarText::new(matter.query_text());
    supply_notes(structure, kinds, pending, &matter, &coordinates);
    let had = kinds.headnotes().map(|(kind, _, _)| kind.to_owned()).collect::<HashSet<_>>();
    let elements = &matter.markup_elements;
    for node in &matter.nodes {
        let range = matter.query_range(node);
        let printed = coordinates.slice_utf16(range.start..range.end).map(squeezed).unwrap_or_default();
        if printed.is_empty() {
            continue;
        }
        if node.role == Some(PrimaryRole::Headnote) {
            let kind = headnote_kind(&matter, node);
            if !had.contains(&kind) { kinds.headnote(&kind, printed, None); }
        } else if let Some(role) = node.role.and_then(front_name).filter(|role| !matches!(*role, "judges" | "counsel")) {
            kinds.front(role, printed, None);
        }
    }
    for element in elements.iter().filter(|element| element.tag == "citation") {
        if let Some(printed) = coordinates.slice_utf16(element.range.start..element.range.end).map(squeezed).filter(|text| !text.is_empty()) {
            kinds.front("citation", printed, None);
        }
    }
}

fn from_text(record: &OpinionRecord, name: &str, text: String, pending: &mut Pending) -> (DocumentStructure, Kinds) {
    let reading = text::read(&text, record.page_count);
    pending.cuts = reading.furniture;
    opinion_document(record, name, text, reading.kinds)
}

fn element_of<'a>(structure: &'a DocumentStructure, node: &StructureNode) -> Option<(usize, &'a MarkupElement)> {
    let at = structure.markup_elements.binary_search_by_key(&node.markup_start?, |element| element.markup_start).ok()?;
    Some((at, &structure.markup_elements[at]))
}

/// What a headnote node is: its element's or an ancestor's kind.
fn headnote_kind(structure: &DocumentStructure, node: &StructureNode) -> String {
    let mut at = element_of(structure, node).map(|(at, _)| at);
    while let Some(index) = at {
        let element = &structure.markup_elements[index];
        let typed = element.attributes.get("data-type").map(String::as_str).unwrap_or(element.tag.as_str());
        if let Some(kind) = ["syllabus", "summary", "headnotes", "history", "seealso", "correction", "arguments", "legal"].iter().find(|kind| typed == **kind) {
            return if *kind == "legal" { "arguments" } else { kind }.to_owned();
        }
        at = element.parent;
    }
    "headnotes".to_owned()
}

/// The opinion read on from its markup's structure (`native_markup`'s CourtListener reader): the parse continues that
/// structure, its text, markup and authorities. The markup's nodes are read first as a whole, while the rules that read
/// them together can (the headnotes and front lines it marks, the bodies of its notes, a source's block counters); then
/// each of its pages, headings, paragraphs, notes and parts is taken out of them into the parse's lists as the opinion's
/// rules read it, and what the markup reads that the opinion does not stays out of its nodes.
fn from_structure(record: &OpinionRecord, name: &str, mut markup: DocumentStructure, pending: &mut Pending) -> (DocumentStructure, Kinds) {
    let nodes = std::mem::take(&mut markup.nodes);
    let index = ScalarCoordinates::new(markup.query_text());
    let structure = &markup;
    let text = structure.query_text();
    let coordinates = ScalarText::with_coordinates(text, &index);
    let slice = |range: ScalarRange| coordinates.slice_utf16(range.start..range.end).map(trim_js).unwrap_or("");

    // The markup's nodes as a whole.
    let body = |note: &StructureNode| if note.ranges.is_empty() { vec![note.range] } else { note.ranges.clone() };
    let note_ranges = nodes.iter().filter(|node| node.kind == NodeKind::Footnote).flat_map(body).collect::<Vec<_>>();
    let mut headnote_nodes = nodes.iter().filter(|node| node.role == Some(PrimaryRole::Headnote)).collect::<Vec<_>>();
    headnote_nodes.sort_by_key(|node| structure.query_range(node).start);
    let headnote_ranges = headnote_nodes.iter().map(|node| structure.query_range(node)).collect::<Vec<_>>();
    // The smallest nodes: a section of them leaves its paragraphs to speak.
    let leaf = |range: &ScalarRange| !headnote_ranges.iter().any(|other| other != range && range.start <= other.start && other.end <= range.end);
    let mut headnotes = headnote_nodes.iter().filter(|node| leaf(&structure.query_range(node))).map(|node| {
        let range = structure.query_range(node);
        headnote_node(&headnote_kind(structure, node), squeezed(slice(range)), range)
    }).collect::<Vec<_>>();
    let mut front = nodes.iter().filter_map(|node| {
        let range = structure.query_range(node);
        let role = node.role.and_then(front_name).or((node.markup_tag.as_deref() == Some("author")).then_some("author"))?;
        Some(front_node(role, squeezed(slice(range)), range))
    }).collect::<Vec<_>>();
    let front_ranges = front.iter().map(|line| line.range).collect::<Vec<_>>();
    let blocks = block_marks(name, structure, &nodes, &coordinates).into_iter().map(|(label, start)| block_counter(label, start, "marked")).collect::<Vec<_>>();

    // Then each node the opinion reads, taken out of them in their order, by its kind.
    let (mut page_nodes, mut heading_nodes, mut block_nodes, mut note_nodes, mut part_nodes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for node in nodes {
        match node.kind {
            NodeKind::Page => page_nodes.push(node),
            NodeKind::Heading => heading_nodes.push(node),
            NodeKind::Paragraph | NodeKind::Prose | NodeKind::Quote => block_nodes.push(node),
            NodeKind::Footnote => note_nodes.push(node),
            NodeKind::Section if node.markup_tag.as_deref().is_some_and(|tag| tag == "opinion" || tag.ends_with("_text")) => part_nodes.push(node),
            _ => {}
        }
    }

    let mut pages = page_nodes.into_iter().map(|mut markup| {
        let range = structure.query_range(&markup);
        let label = markup.label.take().unwrap_or_default();
        let series = series_of(&markup);
        let series = label_run(&label).map_or_else(|| series.clone(), |run| format!("{series}/{run}"));
        let marker = markup.marker_range;
        as_page(taken(markup, NodeKind::Page, range, Derivation::Native), label, series, marker, "markup")
    }).collect::<Vec<_>>();
    opening_pages(structure, &coordinates, &note_ranges, &mut pages);
    element_pages(structure, &coordinates, &note_ranges, &mut pages);
    pages.sort_by_key(|page| page.range.start);
    for page in &mut pages {
        if note_ranges.iter().any(|note| note.start <= page.range.start && page.range.start < note.end) { page.properties.insert("in_note".to_owned(), String::new()); } else { page.properties.remove("in_note"); }
        page.marker_range = page.marker_range.or_else(|| printed_mark(&coordinates, page.range.start, page.range.end, page.label.as_deref().unwrap_or("")));
    }
    pages::never_down(structure, &mut pages);
    let page_marks = pages.iter().filter_map(|page| page.marker_range).collect::<Vec<_>>();
    // A range's words without the page marks in it: "*223III." is "III.".
    let bare = |range: ScalarRange| {
        let mut words = String::new();
        let mut at = range.start;
        let first = page_marks.partition_point(|mark| mark.start < range.start);
        for mark in page_marks[first..].iter().take_while(|mark| mark.end <= range.end) {
            words.push_str(coordinates.slice_utf16(at..mark.start).unwrap_or(""));
            at = mark.end.max(at);
        }
        words.push_str(coordinates.slice_utf16(at..range.end).unwrap_or(""));
        words
    };
    let inside = |ranges: &[ScalarRange], range: ScalarRange| ranges.iter().any(|note| note.start <= range.start && range.end <= note.end);
    let mostly_in = |ranges: &[ScalarRange], range: ScalarRange| {
        ranges.iter().map(|note| note.end.min(range.end).saturating_sub(note.start.max(range.start))).sum::<usize>() * 2 >= (range.end - range.start).max(1)
    };

    let mut headings = Vec::new();
    for markup in heading_nodes {
        let range = markup.heading_range.unwrap_or(structure.query_range(&markup));
        let printed = slice(range);
        if !front_ranges.iter().any(|line| line.start <= range.start && range.end <= line.end) && caption_heading(printed) {
            front.push(front_node("caption", squeezed(printed), range));
            continue;
        }
        let mut heading = taken(markup, NodeKind::Heading, range, Derivation::Native);
        heading.grammar = Some("engine".to_owned());
        headings.push(heading);
    }

    let counted = counts_its_blocks(structure);
    let mut paragraphs = Vec::new();
    for mut markup in block_nodes {
        let mut range = structure.query_range(&markup);
        if mostly_in(&note_ranges, range) || inside(&headnote_ranges, range) || inside(&front_ranges, range) || markup.role.is_some_and(|role| role != PrimaryRole::Text) {
            continue;
        }
        let printed = bare(range);
        if printed.trim().is_empty() {
            continue;
        }
        if headings.iter().any(|heading: &StructureNode| heading.range.start <= range.start && range.end <= heading.range.end) {
            continue;
        }
        if text::contents_line(trim_js(&printed)) {
            front.push(front_node("contents", squeezed(&printed), range));
            continue;
        }
        if !headings.iter().any(|heading| heading.range.start < range.end && range.start < heading.range.end) && markup.marker_range.is_none() {
            let after_a_sentence = paragraphs.last().is_none_or(|last: &StructureNode| !(last.kind == NodeKind::Quote) && text::closed_text(slice(last.range)));
            let opened = trim_js(&printed);
            let whole_title = enumerator_end(opened).is_some_and(|end| title_cased(&opened[end..])) && sentences(opened).len() == 1;
            let alone = whole_title || after_a_sentence && (!text::ended_sentence(opened) || (opened.ends_with('.') && printed.split_whitespace().count() <= 3));
            if heading_line(&printed, alone || emphasized_whole(structure, &markup)) {
                // A number on a title keeps its place; it goes to the text under the title once paragraphs are numbered.
                let title = text::says_paragraph(opened).then(|| text::past_marker(opened)).filter(|title| !title.is_empty())
                    .and_then(|title| Some(range.start + utf16_len(&slice(range)[..slice(range).find(title)?])));
                let Some(start) = title else {
                    headings.push(read_heading(range.start, range.end, "shape"));
                    continue;
                };
                headings.push(read_heading(start, range.end, "shape"));
                range.end = start;
            } else if let Some(end) = run_in(structure, &markup, range, &bare) {
                headings.push(read_heading(range.start, end, "run-in"));
                range.start = end;
            } else {
                let (lead, tail) = fused_titles(coordinates.slice_utf16(range.start..range.end).unwrap_or(""));
                if let Some(lead) = lead {
                    headings.push(read_heading(range.start, range.start + lead, "run-in"));
                    range.start += lead;
                }
                if let Some(tail) = tail {
                    headings.push(read_heading(range.start.max(range.end - tail), range.end, "run-in"));
                    range.end -= tail;
                }
            }
        }
        let quote = markup.kind == NodeKind::Quote || markup.markup_tag.as_deref().is_some_and(|tag| matches!(tag, "blockquote" | "excerpt"));
        let label = if markup.kind == NodeKind::Paragraph && !counted { markup.label.take() } else { None };
        let mut paragraph = taken(markup, if quote { NodeKind::Quote } else { NodeKind::Paragraph }, range, Derivation::Native);
        paragraph.label = label;
        paragraphs.push(paragraph);
    }
    let first_block = paragraphs.iter().map(|paragraph: &StructureNode| paragraph.range.start).min().unwrap_or(0);
    let tables = data_tables(structure, first_block);
    paragraphs.retain(|paragraph| !tables.iter().any(|table| table.range.start <= paragraph.range.start && paragraph.range.end <= table.range.end));
    let classed = classed_front(structure, &coordinates);
    // A line of the foot that carries a paragraph's mark, "¶ 17 SUMMERS, C.J., concur.", is still that paragraph.
    paragraphs.retain(|paragraph| text::says_paragraph(trim_js(slice(paragraph.range)))
        || !classed.iter().any(|line| line.range.start <= paragraph.range.start && paragraph.range.end <= line.range.end + 1));
    front.extend(classed.into_iter().filter(|line| !front_ranges.iter().any(|known| known.start <= line.range.start && line.range.end <= known.end)));
    settle(&mut paragraphs, &headings, &note_ranges);

    // Text no block holds: a div's or a br's lines, set out on their own.
    let mut held = paragraphs.iter().map(|paragraph| paragraph.range)
        .chain(headings.iter().map(|heading| heading.range))
        .chain(note_ranges.iter().copied()).chain(headnote_ranges.iter().copied()).chain(front.iter().map(|line| line.range)).chain(page_marks.iter().copied())
        .collect::<Vec<_>>();
    held.sort_by_key(|range| range.start);
    let mut gaps = Vec::new();
    let mut cursor = 0;
    for range in held.iter().copied().chain(std::iter::once(ScalarRange { start: utf16_len(text), end: utf16_len(text) })) {
        if range.start > cursor {
            gaps.push(ScalarRange { start: cursor, end: range.start });
        }
        cursor = cursor.max(range.end);
    }
    let first_text = paragraphs.iter().map(|paragraph| paragraph.range.start).min().unwrap_or(usize::MAX);
    for gap in gaps {
        let Some(raw) = coordinates.slice_utf16(gap.start..gap.end) else { continue };
        let mut at = gap.start;
        for line in raw.split_inclusive('\n') {
            let width = utf16_len(line);
            let words = trim_js(line);
            let start = at + utf16_len(&line[..line.len() - line.trim_start_matches(javascript_whitespace).len()]);
            let range = ScalarRange { start, end: start + utf16_len(words) };
            at += width;
            if words.is_empty() || bare_label(words) {
                continue;
            }
            let enumerated = words.split_whitespace().next().is_some_and(|first| first.ends_with(['.', ')']) && first.len() <= 5);
            if let Some(role) = front_role(words).filter(|_| start < first_text) {
                front.push(front_node(role, squeezed(words), range));
            } else if heading_line(words, true) && (enumerated || !words.chars().any(char::is_lowercase) || section_keyword(words)) {
                headings.push(read_heading(range.start, range.end, "shape"));
            } else if first_text != usize::MAX && start < first_text {
                front.push(front_node("cover", squeezed(words), range));
            } else if words.split_whitespace().count() >= 4 || words.bytes().any(|byte| byte.is_ascii_digit()) {
                paragraphs.push(read_paragraph(range.start, range.end, None));
            }
        }
    }
    paragraphs.sort_by_key(|paragraph| paragraph.range.start);

    let notes = note_nodes.into_iter().map(|mut markup| {
        let (label, echo) = note_label_of(&coordinates, &markup);
        let mut ranges = if markup.ranges.is_empty() { vec![markup.range] } else { std::mem::take(&mut markup.ranges) };
        if let Some(first) = ranges.first_mut() {
            first.start = (first.start + echo).min(first.end);
        }
        let calls = std::mem::take(&mut markup.calls);
        let range = ScalarRange { start: (markup.range.start + echo).min(markup.range.end), end: markup.range.end };
        as_note(taken(markup, NodeKind::Footnote, range, Derivation::Native), label, ranges, calls)
    }).collect::<Vec<_>>();
    let mut notes = join_citation_fragments(notes, &coordinates);

    static PROMOTION: OnceLock<Regex> = OnceLock::new();
    let promotion = PROMOTION.get_or_init(|| Regex::new(r"(?i)\bsame topic\b|\bkey[- ]?number\b").unwrap());
    let mut cuts = Vec::new();
    notes.retain(|note| {
        let body = slice(note.range);
        let advertised = body.len() <= 200 && promotion.is_match(body);
        if advertised {
            cuts.push(note.range);
        }
        !advertised && !notes_title(body)
    });
    anchored_notes(structure, &coordinates, utf16_len(text), &mut notes);
    word_notes(structure, &coordinates, &mut notes);
    let unmatched = unmatched_calls(structure, &coordinates, &notes);

    let parts = part_nodes.into_iter().map(|markup| {
        let range = structure.query_range(&markup);
        let kind = ["type", "data-type"].iter().find_map(|name| markup.markup_attributes.get(*name)).cloned()
            .or_else(|| markup.markup_tag.as_deref().and_then(|tag| tag.strip_suffix("_text")).map(str::to_owned))
            .or_else(|| record.opinion_type.clone()).unwrap_or_else(|| "opinion".to_owned());
        let author = front.iter().find(|line| role_of(line) == "author" && range.start <= line.range.start && line.range.end <= range.end)
            .and_then(|line| line.display_value.clone());
        as_part(taken(markup, NodeKind::Section, range, Derivation::Native), kind, author)
    }).collect::<Vec<_>>();

    let mut marks = Vec::new();
    for element in structure.markup_elements.iter().filter(|element| element.tag == "bracketnum") {
        match headnote_numbers(slice(element.range)) {
            Some(numbers) => marks.push(headnote_mark(&numbers, element.range.start, element.range.end)),
            None => {
                // Words in the same place: a catchword over the passage.
                let printed = squeezed(slice(element.range));
                if !printed.is_empty() {
                    headnotes.push(headnote_node("catchword", printed, element.range));
                }
            }
        }
    }
    pending.cuts = cuts;
    let mut spans = front;
    spans.extend(headnotes.into_iter().chain(marks).chain(unmatched).chain(blocks));
    let kinds = Kinds { pages, paragraphs, notes, headings, parts, tables, spans, opinion: OpinionReading { rendition: name.to_owned(), ..OpinionReading::default() }, coordinates: index };
    // The markup's structure is the opinion's: its nodes are those the parse read, its reading the opinion's.
    let mut document = markup;
    document.profile = Some(crate::DetectionProfile::CaseLossy);
    document.origins.push(legal_structure_model::Origin { id: ORIGIN.to_owned() });
    (document, kinds)
}

/// Where the `count`th word of a text starts.
fn word_place(text: &str, count: usize) -> usize {
    let mut seen = 0;
    let mut inside = false;
    let mut unit = 0;
    for c in text.chars() {
        if c.is_whitespace() {
            inside = false;
        } else if !inside {
            inside = true;
            seen += 1;
            if seen > count {
                return unit;
            }
        }
        unit += c.len_utf16();
    }
    unit
}

/// Tables of two rows or more past the head; a caption's layout table is neither.
fn data_tables(structure: &DocumentStructure, first_block: usize) -> Vec<StructureNode> {
    let elements = &structure.markup_elements;
    elements.iter().filter(|element| element.tag == "table" && element.range.start >= first_block).filter(|element| {
        !elements.iter().any(|other| other.tag == "table" && other.markup_start < element.markup_start && element.markup_end < other.markup_end)
    }).filter(|table| {
        elements.iter().filter(|row| matches!(row.tag.as_str(), "tr" | "row") && table.markup_start < row.markup_start && row.markup_end <= table.markup_end).count() >= 2
    }).map(|table| read_table(table.range.start, table.range.end)).collect()
}

fn emphasized_whole(structure: &DocumentStructure, node: &StructureNode) -> bool {
    let Some((at, element)) = element_of(structure, node) else { return false };
    if node.source != Derivation::Native { return false; }
    let whole = element.range.end - element.range.start;
    structure.markup_elements[at + 1..].iter().take_while(|child| child.markup_start < element.markup_end)
        .any(|child| matches!(child.tag.as_str(), "em" | "i" | "strong" | "b" | "u" | "center" | "underline")
            && (child.range.end - child.range.start) * 10 >= whole * 9)
        || element.attributes.get("align").is_some_and(|align| align.eq_ignore_ascii_case("center"))
}

/// The number a headnote prints at its head, "[3]" or "3.".
pub fn headnote_number(printed: &str) -> Option<String> {
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    NUMBER.get_or_init(|| Regex::new(r"^(?:\[(\d{1,3})\]|(\d{1,3})\.\s)").unwrap()).captures(printed)
        .and_then(|capture| capture.get(1).or_else(|| capture.get(2))).map(|number| number.as_str().to_owned())
}

/// "[5, 6]", "1, 2.", "[8-8]", "3.": the numbers a headnote mark names.
fn headnote_numbers(printed: &str) -> Option<Vec<u32>> {
    static MARK: OnceLock<Regex> = OnceLock::new();
    // Scanners leave "[3J", "[3|", "¡3]", "f 3 ]", "■ [3]"; old reports print "Syllabus 3.", "Item 5:", "3. 4.".
    if !MARK.get_or_init(|| Regex::new(r"^[^\d\p{L}]*(?:f\s|Item\s|Syllabus\s)?[\[(]?\s*\d{1,3}(?:\s*[-–,.]\s*\d{1,3}|\s+\d{1,3})*\s*[.\])|Jj,:']*$").unwrap()).is_match(printed) {
        return None;
    }
    let values = printed.split(|c: char| !c.is_ascii_digit()).filter_map(|value| value.parse().ok()).collect::<Vec<u32>>();
    (!values.is_empty()).then_some(values)
}

fn word_starts(text: &str) -> Vec<(String, usize)> {
    let mut words = Vec::new();
    let (mut current, mut start, mut unit) = (String::new(), 0, 0);
    for c in text.chars() {
        if c.is_alphanumeric() {
            if current.is_empty() { start = unit; }
            current.extend(c.to_lowercase());
        } else if !current.is_empty() {
            words.push((std::mem::take(&mut current), start));
        }
        unit += c.len_utf16();
    }
    if !current.is_empty() { words.push((current, start)); }
    words
}

/// What another rendition gives that this one lacks: its page numbers, each placed by the five words after it where
/// those stand once in this text; and headings, where its paragraphs read the same.
/// How many words stand for a place in a text.
const PLACE_WORDS: usize = 6;
/// How many of a record's differences a row carries to read.
const SHOWN: usize = 5;

/// The words of a text, each with the `PLACE_WORDS` from it joined.
fn places(text: &str) -> Vec<String> {
    let words = word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>();
    words.windows(PLACE_WORDS).map(|window| window.join(" ")).collect()
}

/// A paragraph's opening words, past any number printed on it.
fn opening_words(text: &str) -> Option<String> {
    let words = word_starts(text).into_iter().map(|word| word.0).skip_while(|word| word.bytes().all(|byte| byte.is_ascii_digit())).take(PLACE_WORDS).collect::<Vec<_>>();
    (words.len() == PLACE_WORDS).then(|| words.join(" "))
}

/// What another rendition offers, kept until ours is final.
#[derive(Clone, Debug, Default)]
struct Elsewhere { name: String, openings: HashSet<String>, places: Vec<String>, pages: Vec<String>, numbers: Vec<String>, notes: Vec<String>, headnotes: Vec<String> }

fn note_paragraphs_elsewhere(witness: &mut Witness, name: &str, other: &DocumentStructure, coordinates: &ScalarText) {
    let printed = |node: &StructureNode| { let range = other.query_range(node); coordinates.slice_utf16(range.start..range.end) };
    let paragraphs = || other.nodes.iter().filter(|node| node.kind == NodeKind::Paragraph);
    witness.elsewhere.push(Elsewhere {
        name: name.to_owned(),
        openings: paragraphs().filter_map(printed).filter_map(opening_words).collect(),
        places: places(other.query_text()),
        pages: other.nodes.iter().filter(|node| node.kind == NodeKind::Page && node.grammar.is_none()).filter_map(|node| Some(node.label.as_deref()?.to_owned())).collect(),
        numbers: if counts_its_blocks(other) { Vec::new() } else { paragraphs().filter_map(|node| Some(node.label.as_deref()?.to_owned())).collect() },
        notes: other.nodes.iter().filter(|node| node.kind == NodeKind::Footnote).filter_map(|note| coordinates.slice_utf16(note.range.start..note.range.end)).filter_map(opening_words).collect(),
        headnotes: other.nodes.iter().filter(|node| node.role == Some(PrimaryRole::Headnote)).filter_map(printed).filter_map(opening_words).collect(),
    });
}

/// Item by item, what another rendition offers against the finished structure. Text counts as not held only in a run of words none
/// of which stand in ours: a scanner's slips break a run long before that.
fn witness_union(kinds: &Kinds, witness: &mut Witness, text: &HashSet<String>) {
    const TEXT_RUN: usize = 40;
    let held_pages = kinds.pages.iter().filter(|page| page.grammar.as_deref() != Some("stray")).map(|page| page.label.as_deref().unwrap_or("")).collect::<HashSet<_>>();
    let held_numbers = kinds.paragraphs.iter().filter_map(|paragraph| paragraph.label.as_deref()).collect::<HashSet<_>>();
    let summaries = kinds.headnotes().flat_map(|(_, _, text)| places(text)).collect::<HashSet<_>>();
    let mut tally = BTreeMap::<&'static str, [usize; 2]>::new();
    let mut samples = Vec::new();
    for other in &witness.elsewhere {
        let mut count = |what: &'static str, label: &str, held: bool| {
            let entry = tally.entry(what).or_default();
            entry[0] += 1;
            if !held {
                entry[1] += 1;
                if samples.len() < 12 { samples.push(format!("{what} {label}")); }
            }
        };
        for page in &other.pages { count("page", page, held_pages.contains(page.as_str())); }
        for number in &other.numbers { count("paragraph number", number, held_numbers.contains(number.as_str())); }
        for note in &other.notes { count("note", note, text.contains(note) || summaries.contains(note)); }
        for headnote in &other.headnotes { count("headnote", headnote, summaries.contains(headnote) || text.contains(headnote)); }
        let (mut run, mut lost) = (0, 0);
        for (at, place) in other.places.iter().chain([&String::new()]).enumerate() {
            if !place.is_empty() && !text.contains(place) {
                run += 1;
                continue;
            }
            if run >= TEXT_RUN {
                count("text block", &other.places[at - run], false);
                lost += run;
            }
            run = 0;
        }
        let words = tally.entry("text words").or_default();
        words[0] += other.places.len();
        words[1] += lost;
    }
    witness.union_test = tally.into_iter().map(|(name, counts)| (name.to_owned(), counts)).collect();
    witness.union_samples = samples;
}

/// Each other rendition that sets paragraphs at all against ours. One with under half our count has lost its breaks and says
/// nothing of ours.
fn witness_paragraphs(structure: &DocumentStructure, kinds: &Kinds, pending: &mut Pending) {
    let Some(witness) = pending.witness.as_mut() else { return };
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let ours = kinds.paragraphs.iter().filter_map(|paragraph| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end)).filter_map(opening_words).collect::<HashSet<_>>();
    let text = places(&structure.text).into_iter().collect::<HashSet<_>>();
    witness_union(kinds, witness, &text);
    for other in std::mem::take(&mut witness.elsewhere) {
        let (name, openings) = (other.name, other.openings);
        if openings.len() * 2 < ours.len() {
            continue;
        }
        let words = other.places.into_iter().collect::<HashSet<_>>();
        let mut run_on = openings.iter().filter(|opening| !ours.contains(*opening) && text.contains(*opening)).cloned().collect::<Vec<_>>();
        let mut split = ours.iter().filter(|opening| !openings.contains(*opening) && words.contains(*opening)).cloned().collect::<Vec<_>>();
        let alike = openings.iter().filter(|opening| ours.contains(*opening)).count();
        witness.paragraph_witness.insert(name, [alike, run_on.len(), openings.len() - alike - run_on.len(), split.len()]);
        run_on.sort_unstable();
        split.sort_unstable();
        witness.paragraphs_run_on.extend(run_on.into_iter().take(SHOWN));
        witness.paragraphs_split.extend(split.into_iter().take(SHOWN));
    }
}

/// `theirs` is the other rendition's text, indexed once for every reading of it.
fn supply_from(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending, name: &str, other: &DocumentStructure, theirs_text: &ScalarText) {
    pending.quoted_elsewhere.extend(other.nodes.iter().filter(|node| node.kind == NodeKind::Quote || node.markup_tag.as_deref().is_some_and(|tag| matches!(tag, "blockquote" | "excerpt")))
        .filter_map(|node| { let range = other.query_range(node); theirs_text.slice_utf16(range.start..range.end) }).filter_map(opening_words));
    let witness = pending.witness.is_some();
    if let Some(witness) = pending.witness.as_mut() {
        note_paragraphs_elsewhere(witness, name, other, theirs_text);
    }
    const SHINGLE: usize = 5;
    // How far apart, in UTF-16 units, two renditions may set one page's mark.
    const SAME_PLACE: usize = 300;
    let have = kinds.pages.iter().map(|page| (page.label.as_deref().unwrap_or("").to_owned(), page.range.start)).collect::<HashMap<_, _>>();
    // One rendition may carry several numberings, a reporter's and a vendor's, each by its page scheme. The one most like ours is
    // compared with ours; each other stands as its own, a parallel printing.
    let mut schemes = BTreeMap::<String, Vec<(String, usize)>>::new();
    for node in other.nodes.iter().filter(|node| node.kind == NodeKind::Page && node.grammar.is_none()) {
        schemes.entry(series_of(node)).or_default().push((node.label.as_deref().unwrap_or("").to_owned(), other.query_range(node).start));
    }
    let closest = schemes.iter().max_by_key(|(_, marks)| (marks.iter().filter(|(label, _)| have.contains_key(label)).count(), marks.len())).map(|(scheme, _)| scheme.clone());
    let marks = closest.as_ref().and_then(|scheme| schemes.remove(scheme)).unwrap_or_default();
    let held_series = kinds.pages.iter().map(|page| page.series.clone()).collect::<HashSet<_>>();
    schemes.retain(|scheme, _| !held_series.contains(scheme));
    let counted = block_marks(name, other, &other.nodes, theirs_text);
    let (named, numbers) = if counts_its_blocks(other) || !counted.is_empty() { (false, Vec::new()) } else { printed_numbers(other, theirs_text) };
    let blocks = if kinds.spans_of("block-counter").next().is_none() { counted } else { Vec::new() };
    // Marks all ours add nothing, and reading where they fall costs a whole index of the text.
    if witness || marks.iter().any(|(label, _)| !have.contains_key(label)) || !numbers.is_empty() || !blocks.is_empty() || !schemes.is_empty() {
        let words = word_starts(&structure.text);
        let theirs = word_starts(other.query_text());
        let shingle = |window: &[(String, usize)]| window.iter().map(|word| word.0.as_str()).collect::<Vec<_>>().join(" ");
        let mut index = HashMap::<String, Option<usize>>::new();
        for (at, window) in words.windows(SHINGLE).enumerate() {
            index.entry(shingle(window)).and_modify(|place| *place = None).or_insert(Some(at));
        }
        // The marker's own number is a word of theirs: look just past it.
        let place_of = |start: usize| {
            let first = theirs.partition_point(|word| word.1 < start);
            (0..8).find_map(|ahead| {
                let at = index.get(&shingle(theirs.get(first + ahead..first + ahead + SHINGLE)?)).copied().flatten()?;
                at.checked_sub(ahead)
            })
        };
        // One reporter's numbering only if the labels in common fall where ours do. How many of theirs we hold says nothing: our
        // markup may print every other page. Two reporters' ranges can overlap, and their kinds.pages must stay apart.
        let common = marks.iter().filter(|(label, _)| have.contains_key(label)).collect::<Vec<_>>();
        let placed = common.iter().filter_map(|(label, start)| Some((have[label], words[place_of(*start)?].1))).collect::<Vec<_>>();
        let agreeing = placed.iter().filter(|(ours, theirs)| ours.abs_diff(*theirs) <= SAME_PLACE).count();
        let shared = agreeing * 2 >= placed.len() && (agreeing >= 2 || common.len() * 2 >= marks.len());
        let missing = marks.iter().filter(|(label, _)| !shared || !have.contains_key(label)).cloned().collect::<Vec<_>>();
        let series = if shared || kinds.pages.is_empty() { kinds.pages.first().map_or("reporter-1".to_owned(), |page| page.series.clone()) } else { name.trim_end_matches(".html").to_owned() };
        let places = in_their_order(missing.iter().map(|mark| place_of(mark.1)).collect());
        let mut unplaced = Vec::new();
        for ((label, _), place) in missing.into_iter().zip(places) {
            match place {
                Some(place) => kinds.pages.push(read_page(label, series.clone(), words[place].1, 0, None, "other-rendition")),
                None => unplaced.push(label),
            }
        }
        place_by_number(structure, kinds, &series, unplaced);
        for (scheme, group) in std::mem::take(&mut schemes) {
            let mut unplaced = Vec::new();
            for ((label, _), place) in group.iter().cloned().zip(in_their_order(group.iter().map(|mark| place_of(mark.1)).collect())) {
                match place {
                    Some(place) => kinds.pages.push(read_page(label, scheme.clone(), words[place].1, 0, None, "other-rendition")),
                    None => unplaced.push(label),
                }
            }
            place_by_number(structure, kinds, &scheme, unplaced);
        }
        if let Some(witness) = pending.witness.as_mut().filter(|_| shared) {
            let ordered = in_their_order(marks.iter().map(|mark| place_of(mark.1)).collect());
            let found = marks.iter().zip(ordered).filter_map(|((label, _), place)| Some((page_number(label)?, words[place?].1))).collect::<Vec<_>>();
            witness_pages(&kinds.pages, witness, &found, SAME_PLACE);
        }
        // Counters another rendition sets, each where its words fall; one whose words we lack, between its neighbours.
        // Scanned texts differ word by word: the nearest words the two share fix the place, after the counter or before it.
        const BLOCK_REACH: usize = 60;
        let place_near = |start: usize| place_of(start).or_else(|| {
            let first = theirs.partition_point(|word| word.1 < start);
            (1..BLOCK_REACH).find_map(|apart| {
                let after = theirs.get(first + apart..first + apart + SHINGLE).and_then(|window| index.get(&shingle(window)).copied().flatten()).and_then(|at| at.checked_sub(apart));
                after.or_else(|| {
                    let from = first.checked_sub(apart + SHINGLE)?;
                    index.get(&shingle(&theirs[from..from + SHINGLE])).copied().flatten().map(|at| at + apart + SHINGLE)
                })
            })
        }).filter(|place| *place < words.len());
        let placed = in_their_order(blocks.iter().map(|block| place_near(block.1)).collect()).into_iter().map(|place| place.map(|place| words[place].1)).collect::<Vec<_>>();
        let whole = utf16_len(&structure.text);
        for (index, (label, _)) in blocks.iter().enumerate() {
            let (start, source) = match placed[index] {
                Some(start) => (start, "other-rendition"),
                None => {
                    let before = placed[..index].iter().enumerate().rev().find_map(|(at, place)| place.map(|place| (at, place))).unwrap_or((0, 0));
                    let after = placed[index + 1..].iter().enumerate().find_map(|(at, place)| place.map(|place| (index + 1 + at, place))).unwrap_or((blocks.len(), whole));
                    (before.1 + (after.1.saturating_sub(before.1)) * (index - before.0) / (after.0 - before.0).max(1), "reckoned")
                }
            };
            kinds.spans.push(block_counter(label.clone(), start, source));
        }
        let printed = numbers.iter().filter_map(|(number, start)| Some((*number, words[place_near(*start)?].1))).collect::<Vec<_>>();
        if !printed.is_empty() {
            pending.printed_elsewhere.push((named, printed));
        }
    }
    let key = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>().join(" ");
    // Tagged a heading, or a whole block set apart by centring or emphasis that reads as a title: "ON MOTION FOR REHEARING."
    let set_apart = |node: &StructureNode| node.kind == NodeKind::Paragraph && emphasized_whole(other, node) && { let range = other.query_range(node); theirs_text.slice_utf16(range.start..range.end).is_some_and(|printed| heading_line(printed, true)) };
    let wanted = other.nodes.iter().filter(|node| node.kind == NodeKind::Heading || set_apart(node))
        .map(|node| other.query_range(node)).collect::<Vec<_>>();
    if wanted.is_empty() {
        return;
    }
    let wanted = wanted.into_iter().filter_map(|range| theirs_text.slice_utf16(range.start..range.end).map(key)).filter(|text| !text.is_empty()).collect::<HashSet<_>>();
    pending.marked_titles.extend(wanted.iter().cloned());
    let mine = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let promoted = kinds.paragraphs.iter().enumerate().filter(|(_, paragraph)| paragraph.label.is_none())
        .filter(|(_, paragraph)| mine.slice_utf16(paragraph.range.start..paragraph.range.end).is_some_and(|text| wanted.contains(&key(text))))
        .map(|(index, _)| index).collect::<Vec<_>>();
    for index in promoted.into_iter().rev() {
        let paragraph = kinds.paragraphs.remove(index);
        kinds.headings.push(read_heading(paragraph.range.start, paragraph.range.end, "other-rendition"));
    }
}

/// Head lines and headnotes this rendition lacks and another prints: a role or a kind of headnote none of ours fills.
fn supply_head(kinds: &mut Kinds, other: &DocumentStructure, coordinates: &ScalarText) {
    let roles = kinds.front_lines().map(|(role, _)| role.to_owned()).collect::<HashSet<_>>();
    let filled = kinds.headnotes().map(|(kind, _, _)| kind.to_owned()).collect::<HashSet<_>>();
    for node in other.nodes.iter().filter(|node| node.role == Some(PrimaryRole::Headnote) || node.role.and_then(front_name).is_some_and(|role| !roles.contains(role))) {
        let range = other.query_range(node);
        let printed = coordinates.slice_utf16(range.start..range.end).map(squeezed).unwrap_or_default();
        if printed.is_empty() {
            continue;
        }
        if node.role == Some(PrimaryRole::Headnote) {
            let kind = headnote_kind(other, node);
            // A kind ours fills may still say more: a union keeps every headnote it does not already hold.
            let known = opening_words(&printed).is_some_and(|opening| kinds.headnotes().any(|(_, _, text)| places(text).contains(&opening)));
            if !filled.contains(&kind) || !known {
                kinds.headnote(&kind, printed, None);
            }
        } else if let Some(role) = node.role.and_then(front_name).filter(|role| !roles.contains(*role)) {
            kinds.front(role, printed, None);
        }
    }
}

/// What the readings leave to do, in order: each step needs the ones before it.
/// A block one rendition sets as a quotation is one in the union, whichever text is read.
fn quotes_from_elsewhere(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending) {
    if pending.quoted_elsewhere.is_empty() {
        return;
    }
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let quoted = kinds.paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).and_then(opening_words).is_some_and(|opening| pending.quoted_elsewhere.contains(&opening))).collect::<Vec<_>>();
    for (paragraph, quoted) in kinds.paragraphs.iter_mut().zip(quoted) {
        if quoted { paragraph.kind = NodeKind::Quote; }
    }
}

/// A note found late, by answering its call, can still stand among the paragraphs: its words belong to the note.
fn notes_out_of_the_body(kinds: &mut Kinds) {
    /// How much of a paragraph must lie in kinds.notes for it to be theirs, in fifths.
    const MOSTLY: usize = 4;
    let mut ranges = kinds.notes.iter().flat_map(|note| note.ranges.iter().copied()).collect::<Vec<_>>();
    ranges.sort_by_key(|range| range.start);
    kinds.paragraphs.retain(|paragraph| {
        let first = ranges.partition_point(|range| range.end <= paragraph.range.start);
        let held = ranges[first..].iter().take_while(|range| range.start < paragraph.range.end).map(|range| range.end.min(paragraph.range.end).saturating_sub(range.start.max(paragraph.range.start))).sum::<usize>();
        held * 5 < (paragraph.range.end - paragraph.range.start) * MOSTLY
    });
}

/// Another rendition's marks set ahead of the opinion's first words, "*508" in a heading our text does not hold, were estimated at
/// its start and named no page. The one just below the first mark placed is the page the opinion opens on; any before it, strays.
fn opening_from_other_renditions(kinds: &mut Kinds) {
    let from_elsewhere = kinds.pages.iter().filter(|page| page.grammar.as_deref() == Some("other-rendition")).map(|page| page.series.clone()).collect::<HashSet<_>>();
    for series in from_elsewhere {
        let first_placed = kinds.pages.iter().filter(|page| page.series == series && page.grammar.as_deref() == Some("other-rendition")).min_by_key(|page| page.range.start).and_then(|page| page_number(page.label.as_deref().unwrap_or("")));
        let Some(first_placed) = first_placed else { continue };
        let ahead = kinds.pages.iter().enumerate().filter(|(_, page)| page.series == series && page.grammar.as_deref() == Some("reckoned") && page.range.start == 0).filter_map(|(at, page)| Some((at, page_number(page.label.as_deref().unwrap_or(""))?))).collect::<Vec<_>>();
        let opening = ahead.iter().filter(|mark| mark.1 + 1 == first_placed).map(|mark| mark.0).next();
        for (at, _) in &ahead {
            kinds.pages[*at].grammar = Some((if Some(*at) == opening { "opening" } else { "stray" }).to_owned());
        }
    }
}

fn finish(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending, record: &OpinionRecord) {
    opening_from_other_renditions(kinds);
    quotes_from_elsewhere(structure, kinds, pending);
    flag_pages_in_notes(kinds);
    page_series(kinds, record);
    close_pages(structure, kinds);
    tidy_paragraphs(structure, kinds, pending);
    label_notes(structure, kinds);
    enumerated_titles(structure, kinds);
    take_marked_titles(structure, kinds, pending);
    title_headings(structure, kinds);
    list_items(kinds);
    sift_headings(kinds);
    witness_headings(structure, kinds, pending);
    witness_paragraphs(structure, kinds, pending);
    divisions_of_headnotes(structure, kinds);
    head_lines(structure, kinds);
    reporter_matter(structure, kinds);
    cover_matter(structure, kinds);
    read_the_text(structure, kinds);
    notes_out_of_the_body(kinds);
    place_on_pages(kinds);
}

/// Marks placed from other renditions fall in notes too, and a note's page says nothing of the opinion's.
fn flag_pages_in_notes(kinds: &mut Kinds) {
    let mut ranges = kinds.notes.iter().flat_map(|note| note.ranges.iter().copied()).collect::<Vec<_>>();
    ranges.sort_by_key(|note| note.start);
    for page in kinds.pages.iter_mut() {
        { let in_note = page.properties.contains_key("in_note") || ranges[..ranges.partition_point(|note| note.start <= page.range.start)].last().is_some_and(|note| page.range.start < note.end); if in_note { page.properties.insert("in_note".to_owned(), String::new()); } else { page.properties.remove("in_note"); } }
    }
}

/// An opinion-only rendition marks the case's first page where the opinion opens, past the marks another carried through the
/// head: 739, 740, 738, 741. Lower than all before it and the run going straight on after it, it heads the text.
fn opening_page_first(kinds: &mut Kinds) {
    let mut lowered = false;
    let series = kinds.pages.iter().enumerate().filter(|(_, page)| !page.properties.contains_key("in_note") && page.series.starts_with("reporter")).fold(BTreeMap::<&str, Vec<(usize, u32)>>::new(), |mut all, (index, page)| {
        if let Some(number) = page_number(page.label.as_deref().unwrap_or("")) { all.entry(&page.series).or_default().push((index, number)); }
        all
    }).into_values().collect::<Vec<_>>();
    let mut opening = Vec::new();
    for run in &series {
        if let Some(at) = (1..run.len()).find(|&at| run[at].1 < run[..at].iter().map(|page| page.1).min().unwrap_or(0)) {
            let highest = run[..at].iter().map(|page| page.1).max().unwrap_or(0);
            if run.get(at + 1).is_none_or(|next| next.1 > highest && next.1 <= highest + 3) {
                opening.push(run[at].0);
            }
        }
    }
    for index in opening {
        kinds.pages[index].range.start = 0;
        lowered = true;
    }
    if lowered {
        kinds.pages.sort_by(|left, right| (&left.series, left.range.start, page_number(left.label.as_deref().unwrap_or(""))).cmp(&(&right.series, right.range.start, page_number(right.label.as_deref().unwrap_or("")))));
    }
}

/// Each numbering's pages in the text's order, each running to the next.
fn close_pages(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    let text_end = utf16_len(&structure.text);
    kinds.pages.sort_by(|left, right| (&left.series, left.range.start, page_number(left.label.as_deref().unwrap_or(""))).cmp(&(&right.series, right.range.start, page_number(right.label.as_deref().unwrap_or("")))));
    // A plain text can print a page's number twice, at its head and its foot: one page.
    if kinds.opinion.rendition == "plain_text.txt" {
        kinds.pages.dedup_by(|later, kept| {
            let same = later.series == kept.series && later.label == kept.label;
            if same { kept.range.end = kept.range.end.max(later.range.end); }
            same
        });
    }
    opening_page_first(kinds);
    // A page opened twice is one page; a moved block's page is its block's alone and leaves the opinion's page as it was.
    let mut opened = HashSet::new();
    kinds.pages.retain(|page| page.properties.contains_key("in_note") || !page.series.starts_with("reporter") || page.grammar.as_deref() == Some("stray") || page.grammar.as_deref() == Some("moved") || opened.insert((page.series.clone(), page.label.as_deref().unwrap_or("").to_owned())));
    let count = kinds.pages.len();
    for index in 0..count {
        if kinds.pages[index].range.end == 0 || matches!(kinds.pages[index].grammar.as_deref(), Some("other-rendition" | "reckoned")) {
            let next = kinds.pages[index + 1..].iter().find(|page| page.series == kinds.pages[index].series).map_or(text_end, |page| page.range.start);
            kinds.pages[index].range.end = next;
        }
    }
    // What other renditions added cuts the page before it.
    for index in 1..count {
        if kinds.pages[index].series == kinds.pages[index - 1].series && kinds.pages[index - 1].range.end > kinds.pages[index].range.start {
            kinds.pages[index - 1].range.end = kinds.pages[index].range.start;
        }
    }
}

fn tidy_paragraphs(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending) {
    let note_ranges = kinds.notes.iter().flat_map(|note| note.ranges.iter().copied()).collect::<Vec<_>>();
    settle(&mut kinds.paragraphs, &kinds.headings, &note_ranges);
    rejoin_paragraphs(structure, kinds);
    drop_empty_paragraphs(structure, kinds);
    hold_leftovers(structure, kinds);
    number_paragraphs(structure, kinds, pending);
    speakers(structure, kinds);
    separate_parts(structure, kinds);
    printed_headnote_marks(structure, kinds);
}

fn label_notes(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    share_note_calls(structure, kinds);
    // Notes a source left unlabelled and unnumbered are numbered as they stand.
    if kinds.notes.iter().all(|note| note.label.as_deref().unwrap_or("").is_empty()) {
        for (index, note) in kinds.notes.iter_mut().enumerate() {
            note.label = Some(format!("{}", index + 1));
        }
    }
    let mut answered = kinds.notes.iter().flat_map(|note| note.calls.iter().copied()).collect::<Vec<_>>();
    answered.sort_by_key(|known| known.start);
    kinds.spans.retain(|call| {
        !reads(call, "unmatched-call") || answered[..answered.partition_point(|known| known.start < call.range.end)].last().is_none_or(|known| known.end <= call.range.start)
    });
}

/// A title's words: where a note's call stands at its end, the words stop before it, for the call is no word of the title.
fn title_words<'a>(coordinates: &ScalarText<'a>, calls: &[ScalarRange], heading: &StructureNode) -> &'a str {
    let within = &calls[calls.partition_point(|call| call.start <= heading.range.start)..];
    let words_end = within.iter().take_while(|call| call.start < heading.range.end).find(|call| call.end + 1 >= heading.range.end).map_or(heading.range.end, |call| call.start);
    coordinates.slice_utf16(heading.range.start..words_end).map(trim_js).unwrap_or("")
}

/// Each heading's words.
fn title_headings(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    let mut calls = kinds.notes.iter().flat_map(|note| note.calls.iter().copied()).collect::<Vec<_>>();
    calls.sort_by_key(|call| call.start);
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    kinds.headings.sort_by_key(|heading| heading.range.start);
    for heading in kinds.headings.iter_mut() {
        let title = paged_title(unfused(title_words(&coordinates, &calls, heading)));
        heading.display_value = Some(letter_spaced(title).unwrap_or_else(|| squeezed(title)));
    }
}

/// Each heading under the title it ranks below, read off the ladder of enumerator families its titles open on: a family
/// keeps its rung, a new one goes under the last, and a title of none starts the ladder again. The finished structure's
/// headings, each with its id.
fn heading_ladder(structure: &mut DocumentStructure) {
    let mut calls = nodes(structure, NodeKind::Footnote).flat_map(|note| note.calls.iter().copied()).collect::<Vec<_>>();
    calls.sort_by_key(|call| call.start);
    let coordinates = ScalarText::new(&structure.text);
    // Each rung's family and the heading last on it.
    let mut ladder = Vec::<(String, &str)>::new();
    let mut letter = None;
    let mut parents = Vec::new();
    for (index, heading) in structure.nodes.iter().enumerate().filter(|(_, node)| node.kind == NodeKind::Heading && node.origin_id == ORIGIN) {
        let printed = title_words(&coordinates, &calls, heading);
        // "C." after "B." is a letter, not a hundred.
        let family = hierarchy::numbering(printed, letter);
        letter = if matches!(family.as_str(), "AU" | "al") { printed.chars().find(|c| c.is_alphabetic()).and_then(|c| c.to_lowercase().next()) } else { letter };
        if family.is_empty() {
            ladder.clear();
            continue;
        }
        let rung = match ladder.iter().position(|(seen, _)| *seen == family) {
            Some(at) => {
                ladder.truncate(at + 1);
                ladder[at].1 = &heading.id;
                at
            }
            None => {
                ladder.push((family, &heading.id));
                ladder.len() - 1
            }
        };
        if let Some(up) = rung.checked_sub(1) {
            parents.push((index, ladder[up].1.to_owned()));
        }
    }
    for (index, parent) in parents {
        structure.nodes[index].parent_id = Some(parent);
    }
}

/// A heading's words decide last: a mark, a rule, a list's title or a contents line is none.
fn sift_headings(kinds: &mut Kinds) {
    for heading in std::mem::take(&mut kinds.headings) {
        let title = heading.display_value.as_deref().unwrap_or("");
        if !title.chars().any(char::is_alphanumeric) {
            continue;
        }
        if text::contents_line(title) {
            kinds.spans.push(front_node("contents", title.to_owned(), heading.range));
        } else if let Some(numbers) = title.starts_with('[').then(|| headnote_numbers(title)).flatten() {
            kinds.spans.push(headnote_mark(&numbers, heading.range.start, heading.range.end));
        } else {
            kinds.headings.push(heading);
        }
    }
}

/// What is read from the finished text and paragraphs.
fn read_the_text(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let searched = kinds.paragraphs.iter().map(|paragraph| (paragraph.range.start, paragraph.range.end))
        .chain(kinds.notes.iter().flat_map(|note| note.ranges.iter().map(|range| (range.start, range.end)))).collect::<Vec<_>>();
    let mut found = defined_terms(&coordinates, &searched);
    found.extend(quotations(&coordinates, &kinds.paragraphs));
    found.extend(instruments(&coordinates, &kinds.paragraphs));
    let gaps = record_gaps(structure, kinds);
    // Blocks past the last paragraph, or lines of the foot, may carry the order the kinds.paragraphs do not.
    let last_text = kinds.paragraphs.iter().map(|paragraph| paragraph.range.start).max().unwrap_or(0);
    let closing = kinds.headings.iter().map(|heading| heading.range)
        .chain(kinds.spans_of("headnote").filter(|headnote| headnote.properties.get("kind").is_some_and(|kind| kind == "topics")).map(|headnote| headnote.range))
        .chain(kinds.spans_of("front").map(|line| line.range))
        .filter(|range| range.start >= last_text)
        // A slip's head may print its disposition: "AFFIRMED."
        .chain(kinds.spans_of("front").filter(|line| role_of(line) == "cover").map(|line| line.range)).collect::<Vec<_>>();
    let (lines, outcomes) = dispositions(&coordinates, &kinds.paragraphs, &kinds.parts, &closing);
    kinds.opinion.record_gaps = gaps;
    kinds.opinion.document_kind = document_kind(&structure.text).to_owned();
    kinds.spans.extend(found.into_iter().chain(lines));
    kinds.opinion.outcomes = outcomes;
    name_dates(kinds);
}

/// How near the next page a note's call may stand, in UTF-16 units (a line and a half), before its note's page is in doubt.
const CALL_AT_FOOT: usize = 120;

/// The lead numbering's page of everything found, its pins in every numbering, and each paragraph's part.
fn place_on_pages(kinds: &mut Kinds) {
    let mut counts = HashMap::<&str, usize>::new();
    for page in kinds.pages.iter().filter(|page| in_flow(page)) {
        *counts.entry(page.series.as_str()).or_default() += 1;
    }
    let lead = counts.iter().max_by_key(|(series, count)| (series.starts_with("reporter"), **count, std::cmp::Reverse(**series))).map(|(series, _)| *series);
    let lead_turns = kinds.pages.iter().filter(|page| in_flow(page)).filter(|page| Some(page.series.as_str()) == lead).map(|page| page.range.start).collect::<Vec<_>>();
    // Each page under the names a pin cites it by: "*702", and "226 La. 702" where its numbering is a printing's.
    for page in kinds.pages.iter_mut() {
        let mut aliases = vec![format!("*{}", page.label.as_deref().unwrap_or(""))];
        aliases.extend(kinds.opinion.page_series.iter().filter(|series| series.series == page.series).map(|series| format!("{} {}", series.reporter, page.label.as_deref().unwrap_or(""))));
        page.aliases = Some(aliases);
    }
    for (index, part) in kinds.parts.iter_mut().enumerate() {
        part.id = format!("opinion-part-{}", index + 1);
    }
    for paragraph in kinds.paragraphs.iter_mut() {
        paragraph.parent_id = kinds.parts.iter().find(|part| part.range.start <= paragraph.range.start && paragraph.range.start < part.range.end).map(|part| part.id.clone());
    }
    // A note's page is its call's: the kinds.notes of a text set at its end are still on the page that calls them. But a call on a
    // page's last line can have its note set on the next page, or run on to it; a source that marks where kinds.notes run on (Harvard)
    // says so, and one that marks no page inside any note cannot: there a call that close to the next page names no page.
    let marks_run_ons = kinds.pages.iter().any(|page| page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray"));
    let next_turn = |offset: usize| lead_turns.get(lead_turns.partition_point(|turn| *turn <= offset)).copied();
    for note in kinds.notes.iter_mut().filter(|note| !marks_run_ons && note.calls.first().is_some_and(|call| next_turn(call.start).is_some_and(|turn| turn - call.start <= CALL_AT_FOOT))) {
        note.properties.insert("call_at_foot".to_owned(), String::new());
    }
}
