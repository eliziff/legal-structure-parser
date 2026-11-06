//! A court opinion's structure as a lawyer queries it, one flat layer over all its stored versions: its pages by
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

pub use checks::{layer_failures, layer_screens, quote_miss, receipt_miss};
use clean::*;
use headings::*;
use matter::*;
use notes::*;
use pages::*;
use paragraphs::*;
use readings::*;

use legal_structure::{
    analyze_native_markup, hierarchy, javascript_whitespace, trim_javascript_whitespace as trim_js, utf16_len, Derivation, DocumentStructure,
    MarkupElement, NativeMarkupInput, NodeKind, PrimaryRole, ScalarRange, ScalarText, StructureNode,
};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

/// Every rendition that is markup, in CourtListener's order of preference.
const MARKUP: [&str; 6] = ["html_with_citations.html", "xml_harvard.xml", "html_columbia.html", "html_lawbox.html", "html_anon_2020.html", "html.html"];

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
    /// Test each page numbering against the marks other renditions print: `page_witness`. A whole index of the text per rendition.
    pub witness: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpinionPage {
    pub label: String,
    /// Which numbering: `reporter-1` (CAP's first citation), a Lexis page scheme, `slip`, `ecf`, `sheet`, or a
    /// rendition's name where its numbers are not this rendition's.
    pub series: String,
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<ScalarRange>,
    pub source: &'static str,
    /// A page a note's own text turns: it says nothing of the opinion's pages.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub in_note: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpinionParagraph {
    /// The label of a list's item that opens it, as printed: "(a)", "2.", "iii)". A paragraph's own `number` is the opinion's; this
    /// is any list's, a finding's or a charge's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// It carries on the paragraph before it, a note at a page's foot standing between them.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub continues: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub quote: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<usize>,
    /// Who speaks, in a transcript or an exchange: "Q.", "A.", "THE COURT", "MR. SMITH".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Its place in every reporter the record numbers, the lead one among them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<Pin>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpinionNote {
    /// The labels of a list inside the note, as printed: a quoted statute's "(1)", a note's own "A.".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    pub label: String,
    pub start: usize,
    pub end: usize,
    pub ranges: Vec<ScalarRange>,
    pub references: Vec<ScalarRange>,
    /// Its call stands in words of the case this text lacks, as a judge's note is called from the head matter.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub called_outside: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    /// Its call's place in every reporter.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<Pin>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpinionHeading {
    pub text: String,
    pub level: usize,
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<Pin>,
    #[serde(skip)]
    line: usize,
}

/// One headnote, syllabus point or summary paragraph. Its place is in `text` where the opinion carries it; the
/// cluster's have only their words.
#[derive(Clone, Debug, Serialize)]
pub struct OpinionHeadnote {
    /// `headnotes`, `syllabus`, `summary`, `arguments`, `history`, `posture`, `procedural_history`, `cross_reference`,
    /// `correction`, `seealso`, `note` (a reporter's or editor's), `held`, `catchword`, `topics`, `topic_index`, `papers`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// The point's subject, where it leads with one: "APPEAL — Time for Perfecting. ..."
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<ScalarRange>,
}

/// A headnote's number printed in the opinion, "[3]" or "[5, 6]", at the passage it keys.
#[derive(Clone, Debug, Serialize)]
pub struct HeadnoteMark {
    pub numbers: Vec<u32>,
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A block counter a source sets, `<div class="num"><span class="num">14</span>`: no number the court printed, but the only one
/// some texts carry, and it may stand on a paragraph, a title or a note. `source`: `marked` in this text, `other-rendition`
/// placed by the words after it, `reckoned` between its neighbours where those words are not in this text.
#[derive(Clone, Debug, Serialize)]
pub struct OpinionBlock {
    pub label: String,
    pub start: usize,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<Pin>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpinionPart {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub start: usize,
    pub end: usize,
}

/// A line of the case's front matter: `court`, `docket`, `date`, `judges`, `counsel`, `caption`, `author`, `title`,
/// `citation` (the case's own), `disposition`, `case_name`, `case_name_full`, `date_filed`, `other_dates`, `nature_of_suit`, `lower_court`,
/// `publication`, `subject`, `addressee`, `contents`, `closing`, `joinder`.
#[derive(Clone, Debug, Serialize)]
pub struct FrontLine {
    pub role: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<ScalarRange>,
}

/// A note's call with no note anywhere in the record: the mark stands, its note was never reported.
#[derive(Clone, Debug, Serialize)]
pub struct UnmatchedCall {
    pub label: String,
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A short name the opinion gives something: `("Debtor")`, `Administrative Procedure Act (APA)`, `"Lease" means`.
#[derive(Clone, Debug, Serialize)]
pub struct DefinedTerm {
    pub term: String,
    /// `parenthetical`, `acronym` or `means`.
    pub form: String,
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// An editor's mark in a quotation.
#[derive(Clone, Debug, Serialize)]
pub struct QuoteMark {
    /// `insertion`, `sic`, `omission`, `omitted-note`, or `emphasis` for the words that say so after it.
    pub kind: String,
    pub start: usize,
    pub end: usize,
}

/// A quotation, set inline or as a block, with the marks an editor made in it.
#[derive(Clone, Debug, Serialize)]
pub struct OpinionQuote {
    pub block: bool,
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<QuoteMark>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A document the opinion reproduces: the quoted passages a lead-in names, "the will provides as follows:".
#[derive(Clone, Debug, Serialize)]
pub struct OpinionInstrument {
    /// `will`, `contract`, `jury-charge`, `verdict`, `testimony`, `letter`, `order`, `statute`, `passage` and the like.
    pub kind: String,
    pub lead_in: String,
    pub start: usize,
    pub end: usize,
}

/// Something the record itself shows is not all here.
#[derive(Clone, Debug, Serialize)]
pub struct RecordGap {
    /// `separate-opinion-not-here`, `opens-midway`, `facts-not-here`, `image-not-here`, `later-ruling-to-come`, `attachment-named`.
    pub kind: String,
    pub evidence: String,
}

/// One reporter's printing of the case.
#[derive(Clone, Debug, Serialize)]
pub struct OpinionCitation {
    pub volume: String,
    pub reporter: String,
    pub page: String,
    /// CourtListener's type: 1 federal, 2 state, 3 regional, 4 specialty, 5 early Supreme Court, 6 Lexis, 7 Westlaw, 8 neutral, 9 journal.
    pub kind: String,
}

impl OpinionCitation {
    fn printed(&self) -> String {
        format!("{} {} {}", self.volume, self.reporter, self.page)
    }
}

/// A numbering of pages and the printing it belongs to: which reporter's pages these are.
#[derive(Clone, Debug, Serialize)]
pub struct PageSeries {
    pub series: String,
    pub citation: String,
    /// Volume and reporter, as a pin is cited: "226 La.".
    pub reporter: String,
    /// Other citations that open on the same page, where the record cannot say which of them it is.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
}

/// Where a passage falls in one numbering: "226 La." at 123, running to 124.
#[derive(Clone, Debug, Serialize)]
pub struct Pin {
    pub series: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    pub page: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
}

/// A data table in the opinion, its rows out of the paragraphs.
#[derive(Clone, Debug, Serialize)]
pub struct OpinionTable {
    pub start: usize,
    pub end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A line the reading text leaves out of its flow: a page's running title, number or stamp, and where it stood.
#[derive(Clone, Debug, Serialize)]
pub struct FurnitureLine {
    pub text: String,
    pub at: usize,
}

#[derive(Debug, Serialize)]
pub struct OpinionLayer {
    pub id: String,
    pub rendition: String,
    pub text: String,
    pub pages: Vec<OpinionPage>,
    pub paragraphs: Vec<OpinionParagraph>,
    pub blocks: Vec<OpinionBlock>,
    pub notes: Vec<OpinionNote>,
    pub headings: Vec<OpinionHeading>,
    pub headnotes: Vec<OpinionHeadnote>,
    pub headnote_marks: Vec<HeadnoteMark>,
    pub parts: Vec<OpinionPart>,
    pub front: Vec<FrontLine>,
    pub furniture: Vec<FurnitureLine>,
    pub unmatched_calls: Vec<UnmatchedCall>,
    pub tables: Vec<OpinionTable>,
    pub defined_terms: Vec<DefinedTerm>,
    pub quotes: Vec<OpinionQuote>,
    pub instruments: Vec<OpinionInstrument>,
    pub record_gaps: Vec<RecordGap>,
    pub citations: Vec<OpinionCitation>,
    pub page_series: Vec<PageSeries>,
    /// What the court did: `affirmed`, `reversed`, `remanded`, `vacated`, `dismissed`, `denied`, `granted`, `modified`.
    pub outcomes: Vec<String>,
    /// `court-opinion` or `attorney-general-opinion`, as the text reads, whatever the record's type says.
    pub document_kind: String,
    /// Other renditions' page marks against this layer's pages, by how the page was found: at its place, a page off, further.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub page_witness: BTreeMap<&'static str, [usize; 3]>,
    /// Notes another text of the case prints, against this reading's before they were supplied: held and called, held with no call,
    /// not held.
    pub note_witness: [usize; 3],
    /// Titles another rendition marks as headings: held here as headings, in this text but no heading, not in this text.
    pub heading_witness: [usize; 3],
    /// Paragraphs other renditions open, against ours: opened alike, inside a paragraph of ours, words not in our text; and
    /// paragraphs of ours opening where the others run on. Only with `witness`.
    pub paragraph_witness: BTreeMap<String, [usize; 4]>,
    /// A few of those run together and those split apart, by their opening words.
    pub paragraphs_run_on: Vec<String>,
    pub paragraphs_split: Vec<String>,
    /// What other renditions offer against the finished layer, by structure: offered, not held. Only with `witness`.
    pub union_test: BTreeMap<&'static str, [usize; 2]>,
    /// A few of those not held, to read: "page 412", "text the words it opens on".
    pub union_samples: Vec<String>,
    /// The titles not held, each as its words.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub titles_not_held: Vec<String>,
    /// The other renditions' marked titles, each as its words, until this reading's own headings are titled.
    #[serde(skip)]
    marked_titles: HashSet<String>,
    #[serde(skip)]
    elsewhere: Vec<Elsewhere>,
    /// The opening words of blocks another rendition sets as quotations.
    #[serde(skip)]
    quoted_elsewhere: HashSet<String>,
    /// Each other rendition's paragraph numbers, each where its words fall in our text.
    #[serde(skip)]
    printed_elsewhere: Vec<(bool, Vec<(u32, usize)>)>,
    /// What is yet to leave the text.
    #[serde(skip)]
    cuts: Vec<ScalarRange>,
    /// Renditions of this record that hold another opinion's text, and were not read.
    pub other_opinion: Vec<String>,
}

fn structure_of(id: &str, plain: &str, markup: &str) -> Option<DocumentStructure> {
    // North Dakota's opinions spell the paragraph sign "[&amp;P&amp;12]".
    let markup = markup.replace("&amp;P&amp;", "¶");
    let input: NativeMarkupInput = serde_json::from_value(serde_json::json!({
        "provider": "courtlistener", "id": id, "url": null, "text": plain, "markup": markup, "citation": null,
    })).ok()?;
    analyze_native_markup(input).ok()
}

/// A court's text set in <pre> blocks: markup in name, the extractor's layout in fact.
/// A resource.org page counts its blocks down the margin (`div.num`, `span.num`): that is no number the opinion prints.
/// The block counters a source sets, `<span class="num">14</span>`, each with where it stands.
fn block_marks(name: &str, structure: &DocumentStructure, coordinates: &ScalarText) -> Vec<(String, usize)> {
    let marks = structure.markup_elements.iter().filter(|element| element.tag == "span" && element.attributes.get("class").is_some_and(|class| class == "num")).filter_map(|element| {
        let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(trim_js)?;
        (!printed.is_empty() && printed.bytes().all(|byte| byte.is_ascii_digit())).then(|| (printed.to_owned(), element.range.start))
    }).collect::<Vec<_>>();
    if marks.is_empty() && name == "html_columbia.html" { bracket_counters(structure, coordinates) } else { marks }
}

/// Columbia's counter on each block, "[1] OPINION", "[2] Kenneth Joe Rider": a count of its blocks, title and all, that the
/// opinion never printed, and that holds on most of the blocks or is some other thing.
fn bracket_counters(structure: &DocumentStructure, coordinates: &ScalarText) -> Vec<(String, usize)> {
    const LEAST: usize = 4;
    static COUNTER: OnceLock<Regex> = OnceLock::new();
    let counter = COUNTER.get_or_init(|| Regex::new(r"^\[(\d{1,3})\]\s+\S").unwrap());
    let blocks = structure.nodes.iter().filter(|node| matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Heading)).filter_map(|node| {
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
fn printed_numbers(structure: &DocumentStructure) -> (bool, Vec<(u32, usize)>) {
    const LEAST: usize = 3;
    let coordinates = ScalarText::new(structure.query_text());
    let nodes = structure.nodes.iter().filter(|node| matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose)).collect::<Vec<_>>();
    let openings = nodes.iter().filter_map(|node| { let range = structure.query_range(node); Some((range.start, coordinates.slice_utf16(range.start..range.end).map(trim_js)?)) }).collect::<Vec<_>>();
    // A named mark outranks a list beside it: "¶ 1 ..." over its "1)", "2)", "3)".
    let of_name = text::own_numbers(openings.iter().enumerate().filter(|(_, (_, opening))| text::says_paragraph(opening)).map(|(index, (_, opening))| (index, *opening)), LEAST);
    let read = if of_name.len() >= LEAST { of_name } else { text::own_numbers(openings.iter().enumerate().map(|(index, (_, opening))| (index, *opening)), LEAST) };
    let named = read.keys().any(|index| text::says_paragraph(openings[*index].1));
    let mut found = if named || read.len() * 2 >= openings.len() { read.into_iter().map(|(index, number)| (openings[index].0, number)).collect::<BTreeMap<_, _>>() } else { BTreeMap::new() };
    found.extend(nodes.iter().filter_map(|node| Some((structure.query_range(node).start, node.label.as_deref()?.strip_prefix("par")?.parse::<u32>().ok()?))));
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

impl OpinionLayer {
    /// The stretches of the text no paragraph, heading, note, headnote or line of the front holds.
    pub(crate) fn unheld_stretches(&self) -> Vec<(usize, usize)> {
        let mut held = self.paragraphs.iter().map(|paragraph| (paragraph.start, paragraph.end))
            .chain(self.headings.iter().map(|heading| (heading.start, heading.end)))
            .chain(self.notes.iter().flat_map(|note| note.ranges.iter().map(|range| (range.start, range.end))))
            .chain(self.headnotes.iter().filter_map(|headnote| headnote.range).map(|range| (range.start, range.end)))
            .chain(self.front.iter().filter_map(|line| line.range).map(|range| (range.start, range.end)))
            .collect::<Vec<_>>();
        held.sort_unstable();
        let whole = utf16_len(&self.text);
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
    pub fn source_faults_left(&self) -> (usize, usize) {
        static LEAD: OnceLock<Regex> = OnceLock::new();
        let lead = LEAD.get_or_init(|| Regex::new(r"^\s{0,8}(\d{1,2})(?:\s|$)").unwrap());
        const RUN: usize = 5;
        let (mut margins, mut run, mut last) = (0, 0, 0);
        for line in self.text.lines().filter(|line| !line.trim().is_empty()) {
            let number = lead.captures(line).and_then(|capture| capture[1].parse::<u32>().ok());
            run = if number.is_some_and(|number| number == last + 1) { run + 1 } else { 1 };
            if run == RUN { margins += RUN; } else if run > RUN { margins += 1; }
            last = number.unwrap_or(0);
        }
        (self.text.matches('\u{3}').count(), margins)
    }

    /// Lines inside notes that open on a paragraph's own mark: a note has run on through the opinion.
    pub fn marks_in_notes(&self) -> usize {
        let coordinates = ScalarText::new(&self.text);
        self.notes.iter().flat_map(|note| note.ranges.iter()).filter_map(|range| coordinates.slice_utf16(range.start..range.end))
            .map(|body| body.lines().skip(1).filter(|line| text::says_paragraph(line.trim_start())).count()).sum()
    }

    /// How much of the text (spaces aside) nothing holds, and how the longest such stretch opens.
    pub fn unheld_text(&self) -> (usize, String) {
        let coordinates = ScalarText::new(&self.text);
        let (mut total, mut longest) = (0, (0, String::new()));
        for (start, end) in self.unheld_stretches() {
            let printed = coordinates.slice_utf16(start..end).map_or("", trim_js);
            let size = printed.chars().filter(|c| !c.is_whitespace()).count();
            total += size;
            if size > longest.0 {
                longest = (size, printed.chars().take(80).collect());
            }
        }
        (total, longest.1)
    }
}

fn set_as_text(markup: &str) -> bool {
    markup.contains("<pre") && !markup.contains("<p>") && !markup.contains("<p ") && !markup.contains("<div class=\"num\"")
}

pub fn opinion_layer(record: &OpinionRecord) -> OpinionLayer {
    let rendition = |name: &str| record.renditions.iter().find(|(file, _)| file == name).map(|(_, text)| text.as_str());
    let plain = rendition("plain_text.txt").unwrap_or("");
    let chosen = MARKUP.iter().copied().find(|name| rendition(name).is_some_and(|markup| !markup.trim().is_empty() && !set_as_text(markup)));
    let mut layer = match chosen.and_then(|name| Some((name, structure_of(&record.id, plain, rendition(name)?)?))) {
        Some((name, structure)) => {
            let mut layer = from_structure(record, name, &structure);
            leaves_to_pages(&mut layer, record);
            for other in MARKUP.iter().copied().filter(|other| *other != name) {
                if let Some(markup) = rendition(other).filter(|markup| !set_as_text(markup)) {
                    // The same text, by CourtListener's own derivation.
                    if name == "html_with_citations.html" && other == "xml_harvard.xml" {
                        continue;
                    }
                    if let Some(structure) = structure_of(&record.id, plain, markup) {
                        if !same_opinion(&layer.text, structure.query_text()) {
                            layer.other_opinion.push(other.to_owned());
                            continue;
                        }
                        supply_from(&mut layer, other, &structure, record.witness);
                        supply_notes(&mut layer, &structure);
                        supply_head(&mut layer, &structure);
                    }
                }
            }
            layer
        }
        None => {
            let (name, text) = if plain.trim().is_empty() {
                let markup = MARKUP.iter().copied().find_map(|name| rendition(name).map(|markup| (name, markup)));
                let text = markup.and_then(|(_, markup)| structure_of(&record.id, "", markup)).map(|structure| text::unshifted(structure.query_text()));
                (markup.map_or("", |pair| pair.0), text.unwrap_or_default())
            } else {
                ("plain_text.txt", text::unshifted(plain))
            };
            from_text(record, name, text)
        }
    };
    if layer.rendition != "plain_text.txt" && !plain.trim().is_empty() {
        supply_plain_pages(&mut layer, &text::unshifted(plain), record.page_count);
    }
    complete(layer, record, plain)
}

/// Where another rendition prints a page at a place in this text, what does this layer say there? At that page's own start in any
/// of its numberings, within `tolerance`, it agrees; otherwise the nearest numbering answers, by how many pages it is off.
fn witness_pages(layer: &mut OpinionLayer, marks: &[(u32, usize)], tolerance: usize) {
    let mut numberings = HashMap::<&str, Vec<(usize, u32, &'static str)>>::new();
    for page in layer.pages.iter().filter(|page| !page.in_note && page.source != "stray") {
        if let Some(number) = page_number(&page.label) {
            numberings.entry(page.series.as_str()).or_default().push((page.start, number, page.source));
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
        layer.page_witness.entry(source).or_default()[verdict] += 1;
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
fn take_marked_titles(layer: &mut OpinionLayer) {
    if layer.marked_titles.is_empty() {
        return;
    }
    let coordinates = ScalarText::new(&layer.text);
    let key = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>().join(" ");
    let ours = layer.headings.iter().filter_map(|heading| coordinates.slice_utf16(heading.start..heading.end)).map(|text| bare_title(&key(text))).collect::<HashSet<_>>();
    let mut wanted = HashMap::<String, Vec<Vec<String>>>::new();
    for title in layer.marked_titles.iter().filter(|title| section_title(title)).map(|title| bare_title(title)).filter(|title| title.len() > 3 && !ours.contains(title)) {
        let words = title.split(' ').map(str::to_owned).collect::<Vec<_>>();
        wanted.entry(words[0].clone()).or_default().push(words);
    }
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(layer.paragraphs.len());
    for mut paragraph in std::mem::take(&mut layer.paragraphs) {
        let raw = coordinates.slice_utf16(paragraph.start..paragraph.end).unwrap_or("");
        let words = word_starts(raw);
        let title = words.first().and_then(|first| wanted.get(&first.0)).and_then(|titles| {
            titles.iter().filter(|title| title.len() <= words.len() && title.iter().zip(&words).all(|(word, ours)| *word == ours.0)).max_by_key(|title| title.len())
        });
        let Some(title) = title.filter(|_| !paragraph.quote && paragraph.number.is_none()) else {
            kept.push(paragraph);
            continue;
        };
        let last = &words[title.len() - 1];
        let end = paragraph.start + last.1 + utf16_len(&last.0);
        taken.push(OpinionHeading { text: String::new(), level: 0, start: paragraph.start + words[0].1, end, page: None, source: "other-rendition", line: 0, pins: Vec::new() });
        if words.len() > title.len() {
            paragraph.start = end;
            kept.push(paragraph);
        }
    }
    layer.paragraphs = kept;
    layer.headings.append(&mut taken);
}

/// The titles other renditions mark, against the headings this reading came to. A case's name and a list of notes are no section's
/// title. A title compares by its words alone, without a page mark at its head or a note's call at its tail, and may stand in two
/// headings here, "IV." over "Conclusion". Of those not held, the words that stand in this text are titles missed.
fn witness_headings(layer: &mut OpinionLayer) {
    let key = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>().join(" ");
    // A title's paragraph or outline number stays off it here: a rendition may print the mark inside what it marks.
    let unnumbered = |title: String| title.split(' ').skip_while(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit())).collect::<Vec<_>>().join(" ");
    let keys = layer.headings.iter().map(|heading| key(&heading.text)).collect::<Vec<_>>();
    let mut ours = keys.iter().map(|title| unnumbered(bare_title(title))).collect::<HashSet<_>>();
    ours.extend(keys.windows(2).map(|pair| unnumbered(bare_title(&format!("{} {}", pair[0], pair[1])))));
    let wanted = layer.marked_titles.iter().filter(|title| section_title(title)).map(|title| unnumbered(bare_title(title))).collect::<HashSet<_>>();
    let unheld = wanted.iter().filter(|title| !ours.contains(*title)).collect::<Vec<_>>();
    let text = if unheld.is_empty() { String::new() } else { format!(" {} ", key(&layer.text)) };
    layer.titles_not_held = unheld.iter().filter(|title| text.contains(&format!(" {title} "))).map(|title| (*title).clone()).collect();
    layer.heading_witness = [wanted.len() - unheld.len(), layer.titles_not_held.len(), unheld.len() - layer.titles_not_held.len()];
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
fn place_by_number(layer: &mut OpinionLayer, series: &str, labels: Vec<String>) {
    let held = |layer: &OpinionLayer| layer.pages.iter().filter(|page| page.series == series).filter_map(|page| Some((page_number(&page.label)?, page.start))).collect::<Vec<_>>();
    let mut known = held(layer).into_iter().map(|page| page.0).collect::<HashSet<_>>();
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
    let pages = held(layer);
    let text_end = utf16_len(&layer.text);
    let word_places = word_starts(&layer.text).into_iter().map(|word| word.1).collect::<Vec<_>>();
    for (number, label) in numbered {
        if !joined.contains(&number) {
            layer.pages.push(OpinionPage { label, series: series.to_owned(), start: text_end, end: text_end, marker: None, source: "stray", in_note: false });
            continue;
        }
        let lowest = joined.iter().copied().min().unwrap_or(number);
        let highest = joined.iter().copied().max().unwrap_or(number);
        let below = pages.iter().filter(|page| page.0 < number).max_by_key(|page| page.0).copied().or(pages.is_empty().then_some((lowest, 0)));
        let above = pages.iter().filter(|page| page.0 > number).min_by_key(|page| page.0);
        let start = below.map_or(0, |low| {
            let (high_number, high_start) = above.map_or((highest + 1, text_end), |high| *high);
            let reckoned = low.1 + high_start.saturating_sub(low.1) * (number - low.0) as usize / (high_number - low.0) as usize;
            word_places.get(word_places.partition_point(|place| *place < reckoned)).copied().unwrap_or(reckoned)
        });
        layer.pages.push(OpinionPage { label, series: series.to_owned(), start, end: 0, marker: None, source: "reckoned", in_note: false });
    }
}

/// The numbering a plain text prints on its pages, where the markup chosen has lost it: each page placed in our text by the
/// words it opens on.
fn supply_plain_pages(layer: &mut OpinionLayer, plain: &str, page_count: Option<u32>) {
    const SHINGLE: usize = 5;
    let pages = text::read(plain, page_count).pages.into_iter().filter(|page| page.series != "sheet").collect::<Vec<_>>();
    if pages.is_empty() || layer.pages.iter().any(|page| pages.iter().any(|ours| ours.series == page.series)) {
        return;
    }
    let shingle = |window: &[(String, usize)]| window.iter().map(|word| word.0.as_str()).collect::<Vec<_>>().join(" ");
    let words = word_starts(&layer.text);
    let mut index = HashMap::<String, Option<usize>>::new();
    for (at, window) in words.windows(SHINGLE).enumerate() {
        index.entry(shingle(window)).and_modify(|place| *place = None).or_insert(Some(at));
    }
    let coordinates = ScalarText::new(plain);
    let places = in_their_order(pages.iter().map(|page| {
        let opening = (0..4).find_map(|short| coordinates.slice_utf16(page.start..(page.start + 400 - short).min(page.end))).unwrap_or("");
        let theirs = word_starts(opening);
        theirs.windows(SHINGLE).enumerate().take(12).find_map(|(ahead, window)| index.get(&shingle(window)).copied().flatten()?.checked_sub(ahead))
    }).collect());
    let mut unplaced = HashMap::<String, Vec<String>>::new();
    for (page, place) in pages.into_iter().zip(places) {
        match place {
            Some(place) => layer.pages.push(OpinionPage { label: page.label, series: page.series, start: words[place].1, end: 0, marker: None, source: "other-rendition", in_note: false }),
            None => unplaced.entry(page.series).or_default().push(page.label),
        }
    }
    for (series, labels) in unplaced {
        place_by_number(layer, &series, labels);
    }
}

/// What every reading of a record goes through alike.
fn complete(mut layer: OpinionLayer, record: &OpinionRecord, plain: &str) -> OpinionLayer {
    let rendition = |name: &str| record.renditions.iter().find(|(file, _)| file == name).map(|(_, text)| text.as_str());
    printed_page_marks(&mut layer, &record.citations);
    flat_notes(&mut layer);
    from_cluster(&mut layer, record);
    split_note_lists(&mut layer);
    answer_calls(&mut layer);
    excise(&mut layer);
    layer.citations = record.citations.clone();
    if layer.headnotes.is_empty() {
        let found = record.renditions.iter().filter(|(_, text)| names_a_digest(text)).find_map(|(name, text)| {
            let read = if name.ends_with(".txt") { text::unshifted(text) } else { structure_of(&record.id, "", text)?.query_text().to_owned() };
            titled_digest(&read)
        });
        layer.headnotes.extend(found.map(|(kind, body)| headnote_of(kind, body, None)));
    }
    // Lawbox alone may hold the case's own citation.
    if !layer.front.iter().any(|line| line.role == "citation") {
        static OWN: OnceLock<Regex> = OnceLock::new();
        let own = OWN.get_or_init(|| Regex::new(r"<center><b>([^<]{5,60}\(\d{4}\))</b></center>").unwrap());
        if let Some(capture) = rendition("html_lawbox.html").and_then(|markup| own.captures(markup)) {
            layer.front.push(FrontLine { role: "citation".to_owned(), text: squeezed(&capture[1]), range: None });
        }
    }
    layer.headnotes.extend(topic_index(plain));
    layer.headnotes.extend(rendition("html_columbia.html").map(embedded_topics).unwrap_or_default());
    if layer.parts.is_empty() {
        layer.parts.push(OpinionPart {
            kind: record.opinion_type.clone().unwrap_or_else(|| "opinion".to_owned()), author: record.author.clone(),
            start: 0, end: utf16_len(&layer.text),
        });
    }
    finish(&mut layer);
    layer
}

/// A text's UTF-16 offsets for byte offsets asked for in rising order, each step counting only what lies between.
struct Units<'a> {
    text: &'a str,
    byte: usize,
    unit: usize,
}

impl<'a> Units<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, byte: 0, unit: 0 }
    }

    fn at(&mut self, byte: usize) -> usize {
        self.unit += utf16_len(&self.text[self.byte..byte]);
        self.byte = byte;
        self.unit
    }
}

/// A number and nothing else, as a note's label stands when its note has been read: "[6]", "24".
fn bare_label(words: &str) -> bool {
    words.len() <= 8 && words.chars().any(|c| c.is_ascii_digit()) && words.chars().all(|c| c.is_ascii_digit() || matches!(c, '[' | ']' | '(' | ')' | '.'))
}

fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn topic_of(text: &str) -> Option<String> {
    static TOPIC: OnceLock<Regex> = OnceLock::new();
    TOPIC.get_or_init(|| Regex::new(r"^(?:\d{1,3}\.\s*)?([^—–.]{2,60}?)\s?[—–]").unwrap()).captures(text).map(|capture| capture[1].to_owned())
}

fn headnote_of(kind: &str, text: String, range: Option<ScalarRange>) -> OpinionHeadnote {
    OpinionHeadnote { kind: kind.to_owned(), number: headnote_number(&text), topic: topic_of(&text), text, range }
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
fn from_cluster(layer: &mut OpinionLayer, record: &OpinionRecord) {
    let field = |name: &str| record.cluster.iter().find(|(field, _)| field == name).map(|(_, value)| value.as_str());
    for kind in ["syllabus", "headnotes", "summary", "arguments", "history", "posture", "procedural_history", "cross_reference", "correction"] {
        if let Some(value) = field(kind) {
            layer.headnotes.extend(field_paragraphs(&record.id, value).into_iter().map(|text| headnote_of(kind, text, None)));
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
            layer.front.push(FrontLine { role: role.to_owned(), text: squeezed(value), range: None });
        }
    }
    let Some(structure) = field("headmatter").and_then(|matter| structure_of(&record.id, "", matter)) else { return };
    supply_notes(layer, &structure);
    let coordinates = ScalarText::new(structure.query_text());
    let had = layer.headnotes.iter().map(|headnote| headnote.kind.clone()).collect::<HashSet<_>>();
    let elements = &structure.markup_elements;
    for node in &structure.nodes {
        let range = structure.query_range(node);
        let printed = coordinates.slice_utf16(range.start..range.end).map(squeezed).unwrap_or_default();
        if printed.is_empty() {
            continue;
        }
        if node.role == Some(PrimaryRole::Headnote) {
            let kind = headnote_kind(&structure, node);
            if !had.contains(&kind) { layer.headnotes.push(headnote_of(&kind, printed, None)); }
        } else if let Some(role) = node.role.and_then(front_name).filter(|role| !matches!(*role, "judges" | "counsel")) {
            layer.front.push(FrontLine { role: role.to_owned(), text: printed, range: None });
        }
    }
    for element in elements.iter().filter(|element| element.tag == "citation") {
        if let Some(printed) = coordinates.slice_utf16(element.range.start..element.range.end).map(squeezed).filter(|text| !text.is_empty()) {
            layer.front.push(FrontLine { role: "citation".to_owned(), text: printed, range: None });
        }
    }
}

fn from_text(record: &OpinionRecord, name: &str, text: String) -> OpinionLayer {
    let reading = text::read(&text, record.page_count);
    OpinionLayer {
        id: record.id.clone(), rendition: name.to_owned(), text, pages: reading.pages, paragraphs: reading.paragraphs, notes: reading.notes,
        headings: reading.headings, headnotes: Vec::new(), headnote_marks: Vec::new(), parts: Vec::new(), front: reading.front,
        furniture: Vec::new(), unmatched_calls: Vec::new(), tables: reading.tables, defined_terms: Vec::new(), quotes: Vec::new(), instruments: Vec::new(), record_gaps: Vec::new(), citations: Vec::new(), page_series: Vec::new(), outcomes: Vec::new(), document_kind: String::new(), blocks: Vec::new(), page_witness: BTreeMap::new(), note_witness: [0; 3], heading_witness: [0; 3], paragraph_witness: BTreeMap::new(), paragraphs_run_on: Vec::new(), paragraphs_split: Vec::new(), union_test: BTreeMap::new(), union_samples: Vec::new(), titles_not_held: Vec::new(), marked_titles: HashSet::new(), elsewhere: Vec::new(), quoted_elsewhere: HashSet::new(), printed_elsewhere: Vec::new(), other_opinion: Vec::new(), cuts: reading.furniture,
    }
}

fn front_name(role: PrimaryRole) -> Option<&'static str> {
    Some(match role {
        PrimaryRole::Caption => "caption",
        PrimaryRole::Judges => "judges",
        PrimaryRole::Counsel => "counsel",
        PrimaryRole::Date => "date",
        PrimaryRole::DocketNumber => "docket",
        PrimaryRole::Court => "court",
        PrimaryRole::Title => "title",
        _ => return None,
    })
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

fn from_structure(record: &OpinionRecord, name: &str, structure: &DocumentStructure) -> OpinionLayer {
    let text = structure.query_text();
    let coordinates = ScalarText::new(text);
    let span = |range: ScalarRange| coordinates.slice_utf16(range.start..range.end).map(trim_js).unwrap_or("");
    let mut pages = structure.nodes.iter().filter(|node| node.kind == NodeKind::Page).map(|node| {
        let range = structure.query_range(node);
        OpinionPage {
            label: node.label.as_deref().unwrap_or("").trim_start_matches("page").to_owned(),
            series: {
                let label = node.label.as_deref().unwrap_or("").trim_start_matches("page");
                let series = if page_source(node) == "printed" && series_of(node) == "reporter" { "slip".to_owned() } else { series_of(node) };
                label_run(label).map_or_else(|| series.clone(), |run| format!("{series}/{run}"))
            },
            start: range.start, end: range.end, marker: node.marker_range, source: page_source(node), in_note: false,
        }
    }).collect::<Vec<_>>();
    let note_ranges = structure.notes.iter().flat_map(|note| note.body_ranges.iter().copied()).collect::<Vec<_>>();
    element_pages(structure, &coordinates, &note_ranges, &mut pages);
    pages.sort_by_key(|page| page.start);
    for page in &mut pages {
        page.in_note = note_ranges.iter().any(|note| note.start <= page.start && page.start < note.end);
        page.marker = page.marker.or_else(|| printed_mark(&coordinates, page.start, page.end, &page.label));
    }
    let page_marks = pages.iter().filter_map(|page| page.marker).collect::<Vec<_>>();
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

    let mut headnote_nodes = structure.nodes.iter().filter(|node| node.role == Some(PrimaryRole::Headnote)).collect::<Vec<_>>();
    headnote_nodes.sort_by_key(|node| structure.query_range(node).start);
    let headnote_ranges = headnote_nodes.iter().map(|node| structure.query_range(node)).collect::<Vec<_>>();
    // The smallest nodes: a section of them leaves its paragraphs to speak.
    let leaf = |range: &ScalarRange| !headnote_ranges.iter().any(|other| other != range && range.start <= other.start && other.end <= range.end);
    let headnotes = headnote_nodes.iter().filter(|node| leaf(&structure.query_range(node))).map(|node| {
        let range = structure.query_range(node);
        headnote_of(&headnote_kind(structure, node), squeezed(span(range)), Some(range))
    }).collect::<Vec<_>>();

    let front = structure.nodes.iter().filter_map(|node| {
        let range = structure.query_range(node);
        let role = node.role.and_then(front_name).or((node.markup_tag.as_deref() == Some("author")).then_some("author"))?;
        Some(FrontLine { role: role.to_owned(), text: squeezed(span(range)), range: Some(range) })
    }).collect::<Vec<_>>();
    let front_ranges = front.iter().filter_map(|line| line.range).collect::<Vec<_>>();

    let mut front = front;
    let mut headings = Vec::new();
    for node in structure.nodes.iter().filter(|node| node.kind == NodeKind::Heading) {
        let range = node.heading_range.unwrap_or(structure.query_range(node));
        let printed = span(range);
        if !front_ranges.iter().any(|line| line.start <= range.start && range.end <= line.end) && caption_heading(printed) {
            front.push(FrontLine { role: "caption".to_owned(), text: squeezed(printed), range: Some(range) });
            continue;
        }
        headings.push(OpinionHeading { text: String::new(), level: 0, start: range.start, end: range.end, page: None, source: "engine", line: 0, pins: Vec::new() });
    }

    let counted = counts_its_blocks(structure);
    let heading_at = |start: usize, end: usize, source: &'static str| OpinionHeading { text: String::new(), level: 0, start, end, page: None, source, line: 0, pins: Vec::new() };
    let mut paragraphs = Vec::new();
    for node in structure.nodes.iter().filter(|node| matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Quote)) {
        let mut range = structure.query_range(node);
        if mostly_in(&note_ranges, range) || inside(&headnote_ranges, range) || inside(&front_ranges, range) || node.role.is_some_and(|role| role != PrimaryRole::Text) {
            continue;
        }
        let printed = bare(range);
        if printed.trim().is_empty() {
            continue;
        }
        if headings.iter().any(|heading| heading.start <= range.start && range.end <= heading.end) {
            continue;
        }
        if text::contents_line(trim_js(&printed)) {
            front.push(FrontLine { role: "contents".to_owned(), text: squeezed(&printed), range: Some(range) });
            continue;
        }
        if !headings.iter().any(|heading| heading.start < range.end && range.start < heading.end) && node.marker_range.is_none() {
            let after_a_sentence = paragraphs.last().is_none_or(|last: &OpinionParagraph| !last.quote && text::ended_sentence(span(ScalarRange { start: last.start, end: last.end })));
            let opened = trim_js(&printed);
            let whole_title = enumerator_end(opened).is_some_and(|end| title_cased(&opened[end..])) && sentences(opened).len() == 1;
            let alone = whole_title || after_a_sentence && (!text::ended_sentence(opened) || (opened.ends_with('.') && printed.split_whitespace().count() <= 3));
            if heading_line(&printed, alone || emphasized_whole(structure, node)) {
                // A number on a title keeps its place; it goes to the text under the title once paragraphs are numbered.
                let title = text::says_paragraph(opened).then(|| text::past_marker(opened)).filter(|title| !title.is_empty())
                    .and_then(|title| Some(range.start + utf16_len(&span(range)[..span(range).find(title)?])));
                let Some(start) = title else {
                    headings.push(heading_at(range.start, range.end, "shape"));
                    continue;
                };
                headings.push(heading_at(start, range.end, "shape"));
                range.end = start;
            } else if let Some(end) = run_in(structure, node, range, &bare) {
                headings.push(heading_at(range.start, end, "run-in"));
                range.start = end;
            } else {
                let (lead, tail) = fused_titles(coordinates.slice_utf16(range.start..range.end).unwrap_or(""));
                if let Some(lead) = lead {
                    headings.push(heading_at(range.start, range.start + lead, "run-in"));
                    range.start += lead;
                }
                if let Some(tail) = tail {
                    headings.push(heading_at(range.start.max(range.end - tail), range.end, "run-in"));
                    range.end -= tail;
                }
            }
        }
        paragraphs.push(OpinionParagraph { continues: false, item: None,
            number: (node.kind == NodeKind::Paragraph && !counted).then(|| node.label.as_deref()?.strip_prefix("par").map(str::to_owned)).flatten(),
            start: range.start, end: range.end, page: None,
            quote: node.kind == NodeKind::Quote || node.markup_tag.as_deref().is_some_and(|tag| matches!(tag, "blockquote" | "excerpt")),
            part: None, speaker: None, pins: Vec::new(),
        });
    }
    let first_block = paragraphs.iter().map(|paragraph: &OpinionParagraph| paragraph.start).min().unwrap_or(0);
    let tables = data_tables(structure, first_block);
    paragraphs.retain(|paragraph| !tables.iter().any(|table| table.start <= paragraph.start && paragraph.end <= table.end));
    let classed = classed_front(structure, &coordinates);
    // A line of the foot that carries a paragraph's mark, "¶ 17 SUMMERS, C.J., concur.", is still that paragraph.
    paragraphs.retain(|paragraph| text::says_paragraph(trim_js(span(ScalarRange { start: paragraph.start, end: paragraph.end })))
        || !classed.iter().any(|line| line.range.is_some_and(|range| range.start <= paragraph.start && paragraph.end <= range.end + 1)));
    front.extend(classed.into_iter().filter(|line| !front_ranges.iter().any(|known| line.range.is_some_and(|range| known.start <= range.start && range.end <= known.end))));
    settle(&mut paragraphs, &headings, &note_ranges);

    // Text no block holds: a div's or a br's lines, set out on their own.
    let mut taken = paragraphs.iter().map(|paragraph| ScalarRange { start: paragraph.start, end: paragraph.end })
        .chain(headings.iter().map(|heading| ScalarRange { start: heading.start, end: heading.end }))
        .chain(note_ranges.iter().copied()).chain(headnote_ranges.iter().copied()).chain(front.iter().filter_map(|line| line.range)).chain(page_marks.iter().copied())
        .collect::<Vec<_>>();
    taken.sort_by_key(|range| range.start);
    let mut gaps = Vec::new();
    let mut cursor = 0;
    for range in taken.iter().copied().chain(std::iter::once(ScalarRange { start: utf16_len(text), end: utf16_len(text) })) {
        if range.start > cursor {
            gaps.push(ScalarRange { start: cursor, end: range.start });
        }
        cursor = cursor.max(range.end);
    }
    let first_text = paragraphs.iter().map(|paragraph| paragraph.start).min().unwrap_or(usize::MAX);
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
            if start < first_text && front_role(words).is_some() {
                front.push(FrontLine { role: front_role(words).unwrap().to_owned(), text: squeezed(words), range: Some(range) });
            } else if heading_line(words, true) && (enumerated || !words.chars().any(char::is_lowercase) || section_keyword(words)) {
                headings.push(heading_at(range.start, range.end, "shape"));
            } else if first_text != usize::MAX && start < first_text {
                front.push(FrontLine { role: "cover".to_owned(), text: squeezed(words), range: Some(range) });
            } else if words.split_whitespace().count() >= 4 || words.bytes().any(|byte| byte.is_ascii_digit()) {
                paragraphs.push(OpinionParagraph { continues: false, item: None, number: None, start: range.start, end: range.end, page: None, quote: false, part: None, speaker: None, pins: Vec::new() });
            }
        }
    }
    paragraphs.sort_by_key(|paragraph| paragraph.start);
    let blocks = block_marks(name, structure, &coordinates).into_iter().map(|(label, start)| OpinionBlock { label, start, source: "marked", pins: Vec::new() }).collect::<Vec<_>>();

    let notes = structure.notes.iter().map(|note| {
        let (label, echo) = note_label_of(structure, &coordinates, note);
        let mut ranges = note.body_ranges.clone();
        if let Some(first) = ranges.first_mut() {
            first.start = (first.start + echo).min(first.end);
        }
        OpinionNote {
            items: Vec::new(), label, start: (note.body_range.start + echo).min(note.body_range.end), end: note.body_range.end, ranges,
            references: note.references.iter().map(|reference| reference.range).collect(), page: None, called_outside: false, pins: Vec::new(),
        }
    }).collect::<Vec<_>>();
    let mut notes = join_citation_fragments(notes, &coordinates);

    static PROMOTION: OnceLock<Regex> = OnceLock::new();
    let promotion = PROMOTION.get_or_init(|| Regex::new(r"(?i)\bsame topic\b|\bkey[- ]?number\b").unwrap());
    let mut cuts = Vec::new();
    notes.retain(|note| {
        let body = coordinates.slice_utf16(note.start..note.end).map(trim_js).unwrap_or("");
        let advertised = body.len() <= 200 && promotion.is_match(body);
        if advertised {
            cuts.push(ScalarRange { start: note.start, end: note.end });
        }
        !advertised && !notes_title(body)
    });
    anchored_notes(structure, &coordinates, utf16_len(text), &mut notes);
    word_notes(structure, &coordinates, &mut notes);
    let unmatched_calls = unmatched_calls(structure, &coordinates, &notes);

    let parts = structure.nodes.iter().filter(|node| node.kind == NodeKind::Section && node.markup_tag.as_deref().is_some_and(|tag| tag == "opinion" || tag.ends_with("_text"))).map(|node| {
        let range = structure.query_range(node);
        let kind = ["type", "data-type"].iter().find_map(|name| node.markup_attributes.get(*name)).cloned()
            .or_else(|| node.markup_tag.as_deref().and_then(|tag| tag.strip_suffix("_text")).map(str::to_owned))
            .or_else(|| record.opinion_type.clone()).unwrap_or_else(|| "opinion".to_owned());
        let author = front.iter().find(|line| line.role == "author" && line.range.is_some_and(|line| range.start <= line.start && line.end <= range.end))
            .map(|line| line.text.clone());
        OpinionPart { kind, author, start: range.start, end: range.end }
    }).collect();

    let mut headnotes = headnotes;
    let marks = structure.markup_elements.iter().filter(|element| element.tag == "bracketnum").filter_map(|element| {
        let Some(numbers) = headnote_numbers(span(element.range)) else {
            // Words in the same place: a catchword over the passage.
            let printed = squeezed(span(element.range));
            if !printed.is_empty() {
                headnotes.push(headnote_of("catchword", printed, Some(element.range)));
            }
            return None;
        };
        Some(HeadnoteMark { numbers, start: element.range.start, end: element.range.end, page: None })
    }).collect();
    OpinionLayer {
        id: record.id.clone(), rendition: name.to_owned(), text: text.to_owned(), pages, paragraphs, notes, headings, headnotes,
        headnote_marks: marks, parts, front, furniture: Vec::new(), unmatched_calls, tables, defined_terms: Vec::new(), quotes: Vec::new(), instruments: Vec::new(), record_gaps: Vec::new(), citations: Vec::new(), page_series: Vec::new(), outcomes: Vec::new(), document_kind: String::new(), blocks, page_witness: BTreeMap::new(), note_witness: [0; 3], heading_witness: [0; 3], paragraph_witness: BTreeMap::new(), paragraphs_run_on: Vec::new(), paragraphs_split: Vec::new(), union_test: BTreeMap::new(), union_samples: Vec::new(), titles_not_held: Vec::new(), marked_titles: HashSet::new(), elsewhere: Vec::new(), quoted_elsewhere: HashSet::new(), printed_elsewhere: Vec::new(), other_opinion: Vec::new(), cuts,
    }
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
fn data_tables(structure: &DocumentStructure, first_block: usize) -> Vec<OpinionTable> {
    let elements = &structure.markup_elements;
    elements.iter().filter(|element| element.tag == "table" && element.range.start >= first_block).filter(|element| {
        !elements.iter().any(|other| other.tag == "table" && other.markup_start < element.markup_start && element.markup_end < other.markup_end)
    }).filter(|table| {
        elements.iter().filter(|row| matches!(row.tag.as_str(), "tr" | "row") && table.markup_start < row.markup_start && row.markup_end <= table.markup_end).count() >= 2
    }).map(|table| OpinionTable { start: table.range.start, end: table.range.end, page: None }).collect()
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

fn headnote_number(printed: &str) -> Option<String> {
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

fn note_paragraphs_elsewhere(layer: &mut OpinionLayer, name: &str, other: &DocumentStructure) {
    let coordinates = ScalarText::new(other.query_text());
    let printed = |node: &StructureNode| { let range = other.query_range(node); coordinates.slice_utf16(range.start..range.end) };
    let paragraphs = || other.nodes.iter().filter(|node| node.kind == NodeKind::Paragraph);
    layer.elsewhere.push(Elsewhere {
        name: name.to_owned(),
        openings: paragraphs().filter_map(printed).filter_map(opening_words).collect(),
        places: places(other.query_text()),
        pages: other.nodes.iter().filter(|node| node.kind == NodeKind::Page && node.grammar.is_none()).filter_map(|node| Some(node.label.as_deref()?.trim_start_matches("page").to_owned())).collect(),
        numbers: if counts_its_blocks(other) { Vec::new() } else { paragraphs().filter_map(|node| Some(node.label.as_deref()?.strip_prefix("par")?.to_owned())).collect() },
        notes: other.notes.iter().filter_map(|note| coordinates.slice_utf16(note.body_range.start..note.body_range.end)).filter_map(opening_words).collect(),
        headnotes: other.nodes.iter().filter(|node| node.role == Some(PrimaryRole::Headnote)).filter_map(printed).filter_map(opening_words).collect(),
    });
}

/// Item by item, what another rendition offers against the finished layer. Text counts as not held only in a run of words none
/// of which stand in ours: a scanner's slips break a run long before that.
fn witness_union(layer: &mut OpinionLayer, text: &HashSet<String>) {
    const TEXT_RUN: usize = 40;
    let held_pages = layer.pages.iter().filter(|page| page.source != "stray").map(|page| page.label.as_str()).collect::<HashSet<_>>();
    let held_numbers = layer.paragraphs.iter().filter_map(|paragraph| paragraph.number.as_deref()).collect::<HashSet<_>>();
    let summaries = layer.headnotes.iter().flat_map(|headnote| places(&headnote.text)).collect::<HashSet<_>>();
    let mut tally = BTreeMap::<&'static str, [usize; 2]>::new();
    let mut samples = Vec::new();
    for other in &layer.elsewhere {
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
    layer.union_test = tally;
    layer.union_samples = samples;
}

/// Each other rendition that sets paragraphs at all against ours. One with under half our count has lost its breaks and says
/// nothing of ours.
fn witness_paragraphs(layer: &mut OpinionLayer) {
    let coordinates = ScalarText::new(&layer.text);
    let ours = layer.paragraphs.iter().filter_map(|paragraph| coordinates.slice_utf16(paragraph.start..paragraph.end)).filter_map(opening_words).collect::<HashSet<_>>();
    let text = places(&layer.text).into_iter().collect::<HashSet<_>>();
    witness_union(layer, &text);
    for other in std::mem::take(&mut layer.elsewhere) {
        let (name, openings) = (other.name, other.openings);
        if openings.len() * 2 < ours.len() {
            continue;
        }
        let words = other.places.into_iter().collect::<HashSet<_>>();
        let mut run_on = openings.iter().filter(|opening| !ours.contains(*opening) && text.contains(*opening)).cloned().collect::<Vec<_>>();
        let mut split = ours.iter().filter(|opening| !openings.contains(*opening) && words.contains(*opening)).cloned().collect::<Vec<_>>();
        let alike = openings.iter().filter(|opening| ours.contains(*opening)).count();
        layer.paragraph_witness.insert(name, [alike, run_on.len(), openings.len() - alike - run_on.len(), split.len()]);
        run_on.sort_unstable();
        split.sort_unstable();
        layer.paragraphs_run_on.extend(run_on.into_iter().take(SHOWN));
        layer.paragraphs_split.extend(split.into_iter().take(SHOWN));
    }
}

fn supply_from(layer: &mut OpinionLayer, name: &str, other: &DocumentStructure, witness: bool) {
    {
        let coordinates = ScalarText::new(other.query_text());
        layer.quoted_elsewhere.extend(other.nodes.iter().filter(|node| node.kind == NodeKind::Quote || node.markup_tag.as_deref().is_some_and(|tag| matches!(tag, "blockquote" | "excerpt")))
            .filter_map(|node| { let range = other.query_range(node); coordinates.slice_utf16(range.start..range.end) }).filter_map(opening_words));
    }
    if witness {
        note_paragraphs_elsewhere(layer, name, other);
    }
    const SHINGLE: usize = 5;
    // How far apart, in UTF-16 units, two renditions may set one page's mark.
    const SAME_PLACE: usize = 300;
    let have = layer.pages.iter().map(|page| (page.label.clone(), page.start)).collect::<HashMap<_, _>>();
    // One rendition may carry several numberings, a reporter's and a vendor's, each by its page scheme. The one most like ours is
    // compared with ours; each other stands as its own, a parallel printing.
    let mut schemes = BTreeMap::<String, Vec<(String, usize)>>::new();
    for node in other.nodes.iter().filter(|node| node.kind == NodeKind::Page && node.grammar.is_none()) {
        schemes.entry(series_of(node)).or_default().push((node.label.as_deref().unwrap_or("").trim_start_matches("page").to_owned(), other.query_range(node).start));
    }
    let closest = schemes.iter().max_by_key(|(_, marks)| (marks.iter().filter(|(label, _)| have.contains_key(label)).count(), marks.len())).map(|(scheme, _)| scheme.clone());
    let marks = closest.as_ref().and_then(|scheme| schemes.remove(scheme)).unwrap_or_default();
    let held_series = layer.pages.iter().map(|page| page.series.clone()).collect::<HashSet<_>>();
    schemes.retain(|scheme, _| !held_series.contains(scheme));
    let counted = block_marks(name, other, &ScalarText::new(other.query_text()));
    let (named, numbers) = if counts_its_blocks(other) || !counted.is_empty() { (false, Vec::new()) } else { printed_numbers(other) };
    let blocks = if layer.blocks.is_empty() { counted } else { Vec::new() };
    // Marks all ours add nothing, and reading where they fall costs a whole index of the text.
    if witness || marks.iter().any(|(label, _)| !have.contains_key(label)) || !numbers.is_empty() || !blocks.is_empty() || !schemes.is_empty() {
        let words = word_starts(&layer.text);
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
        // markup may print every other page. Two reporters' ranges can overlap, and their pages must stay apart.
        let common = marks.iter().filter(|(label, _)| have.contains_key(label)).collect::<Vec<_>>();
        let placed = common.iter().filter_map(|(label, start)| Some((have[label], words[place_of(*start)?].1))).collect::<Vec<_>>();
        let agreeing = placed.iter().filter(|(ours, theirs)| ours.abs_diff(*theirs) <= SAME_PLACE).count();
        let shared = agreeing * 2 >= placed.len() && (agreeing >= 2 || common.len() * 2 >= marks.len());
        let missing = marks.iter().filter(|(label, _)| !shared || !have.contains_key(label)).cloned().collect::<Vec<_>>();
        let series = if shared || layer.pages.is_empty() { layer.pages.first().map_or("reporter-1".to_owned(), |page| page.series.clone()) } else { name.trim_end_matches(".html").to_owned() };
        let places = in_their_order(missing.iter().map(|mark| place_of(mark.1)).collect());
        let mut unplaced = Vec::new();
        for ((label, _), place) in missing.into_iter().zip(places) {
            match place {
                Some(place) => layer.pages.push(OpinionPage { label, series: series.clone(), start: words[place].1, end: 0, marker: None, source: "other-rendition", in_note: false }),
                None => unplaced.push(label),
            }
        }
        place_by_number(layer, &series, unplaced);
        for (scheme, group) in std::mem::take(&mut schemes) {
            let mut unplaced = Vec::new();
            for ((label, _), place) in group.iter().cloned().zip(in_their_order(group.iter().map(|mark| place_of(mark.1)).collect())) {
                match place {
                    Some(place) => layer.pages.push(OpinionPage { label, series: scheme.clone(), start: words[place].1, end: 0, marker: None, source: "other-rendition", in_note: false }),
                    None => unplaced.push(label),
                }
            }
            place_by_number(layer, &scheme, unplaced);
        }
        if witness && shared {
            let ordered = in_their_order(marks.iter().map(|mark| place_of(mark.1)).collect());
            let found = marks.iter().zip(ordered).filter_map(|((label, _), place)| Some((page_number(label)?, words[place?].1))).collect::<Vec<_>>();
            witness_pages(layer, &found, SAME_PLACE);
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
        let whole = utf16_len(&layer.text);
        for (index, (label, _)) in blocks.iter().enumerate() {
            let (start, source) = match placed[index] {
                Some(start) => (start, "other-rendition"),
                None => {
                    let before = placed[..index].iter().enumerate().rev().find_map(|(at, place)| place.map(|place| (at, place))).unwrap_or((0, 0));
                    let after = placed[index + 1..].iter().enumerate().find_map(|(at, place)| place.map(|place| (index + 1 + at, place))).unwrap_or((blocks.len(), whole));
                    (before.1 + (after.1.saturating_sub(before.1)) * (index - before.0) / (after.0 - before.0).max(1), "reckoned")
                }
            };
            layer.blocks.push(OpinionBlock { label: label.clone(), start, source, pins: Vec::new() });
        }
        let printed = numbers.iter().filter_map(|(number, start)| Some((*number, words[place_near(*start)?].1))).collect::<Vec<_>>();
        if !printed.is_empty() {
            layer.printed_elsewhere.push((named, printed));
        }
    }
    let key = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>().join(" ");
    let coordinates = ScalarText::new(other.query_text());
    // Tagged a heading, or a whole block set apart by centring or emphasis that reads as a title: "ON MOTION FOR REHEARING."
    let set_apart = |node: &StructureNode| node.kind == NodeKind::Paragraph && emphasized_whole(other, node) && { let range = other.query_range(node); coordinates.slice_utf16(range.start..range.end).is_some_and(|printed| heading_line(printed, true)) };
    let wanted = other.nodes.iter().filter(|node| node.kind == NodeKind::Heading || set_apart(node))
        .map(|node| other.query_range(node)).collect::<Vec<_>>();
    if wanted.is_empty() {
        return;
    }
    let wanted = wanted.into_iter().filter_map(|range| coordinates.slice_utf16(range.start..range.end).map(key)).filter(|text| !text.is_empty()).collect::<HashSet<_>>();
    layer.marked_titles.extend(wanted.iter().cloned());
    let mine = ScalarText::new(&layer.text);
    let promoted = layer.paragraphs.iter().enumerate().filter(|(_, paragraph)| paragraph.number.is_none())
        .filter(|(_, paragraph)| mine.slice_utf16(paragraph.start..paragraph.end).is_some_and(|text| wanted.contains(&key(text))))
        .map(|(index, _)| index).collect::<Vec<_>>();
    for index in promoted.into_iter().rev() {
        let paragraph = layer.paragraphs.remove(index);
        layer.headings.push(OpinionHeading { text: String::new(), level: 0, start: paragraph.start, end: paragraph.end, page: None, source: "other-rendition", line: 0, pins: Vec::new() });
    }
}

/// Head lines and headnotes this rendition lacks and another prints: a role or a kind of headnote none of ours fills.
fn supply_head(layer: &mut OpinionLayer, other: &DocumentStructure) {
    let coordinates = ScalarText::new(other.query_text());
    let roles = layer.front.iter().map(|line| line.role.clone()).collect::<HashSet<_>>();
    let kinds = layer.headnotes.iter().map(|headnote| headnote.kind.clone()).collect::<HashSet<_>>();
    for node in other.nodes.iter().filter(|node| node.role == Some(PrimaryRole::Headnote) || node.role.and_then(front_name).is_some_and(|role| !roles.contains(role))) {
        let range = other.query_range(node);
        let printed = coordinates.slice_utf16(range.start..range.end).map(squeezed).unwrap_or_default();
        if printed.is_empty() {
            continue;
        }
        if node.role == Some(PrimaryRole::Headnote) {
            let kind = headnote_kind(other, node);
            // A kind ours fills may still say more: a union keeps every headnote it does not already hold.
            let known = opening_words(&printed).is_some_and(|opening| layer.headnotes.iter().any(|headnote| places(&headnote.text).contains(&opening)));
            if !kinds.contains(&kind) || !known {
                layer.headnotes.push(headnote_of(&kind, printed, None));
            }
        } else if let Some(role) = node.role.and_then(front_name).filter(|role| !roles.contains(*role)) {
            layer.front.push(FrontLine { role: role.to_owned(), text: printed, range: None });
        }
    }
}

/// What the readings leave to do, in order: each step needs the ones before it.
/// A block one rendition sets as a quotation is one in the union, whichever text is read.
fn quotes_from_elsewhere(layer: &mut OpinionLayer) {
    if layer.quoted_elsewhere.is_empty() {
        return;
    }
    let coordinates = ScalarText::new(&layer.text);
    let quoted = layer.paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.start..paragraph.end).and_then(opening_words).is_some_and(|opening| layer.quoted_elsewhere.contains(&opening))).collect::<Vec<_>>();
    for (paragraph, quoted) in layer.paragraphs.iter_mut().zip(quoted) {
        paragraph.quote |= quoted;
    }
}

fn finish(layer: &mut OpinionLayer) {
    quotes_from_elsewhere(layer);
    flag_pages_in_notes(layer);
    page_series(layer);
    close_pages(layer);
    tidy_paragraphs(layer);
    label_notes(layer);
    enumerated_titles(layer);
    take_marked_titles(layer);
    title_headings(layer);
    list_items(layer);
    sift_headings(layer);
    witness_headings(layer);
    witness_paragraphs(layer);
    divisions_of_headnotes(layer);
    head_lines(layer);
    reporter_matter(layer);
    cover_matter(layer);
    read_the_text(layer);
    place_on_pages(layer);
}

/// Marks placed from other renditions fall in notes too, and a note's page says nothing of the opinion's.
fn flag_pages_in_notes(layer: &mut OpinionLayer) {
    let mut ranges = layer.notes.iter().flat_map(|note| note.ranges.iter().copied()).collect::<Vec<_>>();
    ranges.sort_by_key(|note| note.start);
    for page in &mut layer.pages {
        page.in_note |= ranges[..ranges.partition_point(|note| note.start <= page.start)].last().is_some_and(|note| page.start < note.end);
    }
}

/// An opinion-only rendition marks the case's first page where the opinion opens, past the marks another carried through the
/// head: 739, 740, 738, 741. Lower than all before it and the run going straight on after it, it heads the text.
fn opening_page_first(layer: &mut OpinionLayer) {
    let mut lowered = false;
    let series = layer.pages.iter().enumerate().filter(|(_, page)| !page.in_note && page.series.starts_with("reporter")).fold(BTreeMap::<&str, Vec<(usize, u32)>>::new(), |mut all, (index, page)| {
        if let Some(number) = page_number(&page.label) { all.entry(&page.series).or_default().push((index, number)); }
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
        layer.pages[index].start = 0;
        lowered = true;
    }
    if lowered {
        layer.pages.sort_by(|left, right| (&left.series, left.start, page_number(&left.label)).cmp(&(&right.series, right.start, page_number(&right.label))));
    }
}

/// Each numbering's pages in the text's order, each running to the next.
fn close_pages(layer: &mut OpinionLayer) {
    let text_end = utf16_len(&layer.text);
    layer.pages.sort_by(|left, right| (&left.series, left.start, page_number(&left.label)).cmp(&(&right.series, right.start, page_number(&right.label))));
    // A plain text can print a page's number twice, at its head and its foot: one page.
    if layer.rendition == "plain_text.txt" {
        layer.pages.dedup_by(|later, kept| {
            let same = later.series == kept.series && later.label == kept.label;
            if same { kept.end = kept.end.max(later.end); }
            same
        });
    }
    opening_page_first(layer);
    let mut opened = HashSet::new();
    let pages = std::mem::take(&mut layer.pages);
    layer.pages = pages.into_iter().filter(|page| page.in_note || !page.series.starts_with("reporter") || page.source == "stray" || opened.insert((page.series.clone(), page.label.clone()))).collect();
    let count = layer.pages.len();
    for index in 0..count {
        if layer.pages[index].end == 0 || matches!(layer.pages[index].source, "other-rendition" | "reckoned") {
            let next = layer.pages[index + 1..].iter().find(|page| page.series == layer.pages[index].series).map_or(text_end, |page| page.start);
            layer.pages[index].end = next;
        }
    }
    // What other renditions added cuts the page before it.
    for index in 1..count {
        if layer.pages[index].series == layer.pages[index - 1].series && layer.pages[index - 1].end > layer.pages[index].start {
            layer.pages[index - 1].end = layer.pages[index].start;
        }
    }
}

fn tidy_paragraphs(layer: &mut OpinionLayer) {
    let note_ranges = layer.notes.iter().flat_map(|note| note.ranges.iter().copied()).collect::<Vec<_>>();
    settle(&mut layer.paragraphs, &layer.headings, &note_ranges);
    rejoin_paragraphs(layer);
    drop_empty_paragraphs(layer);
    hold_leftovers(layer);
    number_paragraphs(layer);
    speakers(layer);
    separate_parts(layer);
    printed_headnote_marks(layer);
}

fn label_notes(layer: &mut OpinionLayer) {
    share_note_calls(layer);
    // Notes a source left unlabelled and unnumbered are numbered as they stand.
    if layer.notes.iter().all(|note| note.label.is_empty()) {
        for (index, note) in layer.notes.iter_mut().enumerate() {
            note.label = (index + 1).to_string();
        }
    }
    let mut answered = layer.notes.iter().flat_map(|note| note.references.iter().copied()).collect::<Vec<_>>();
    answered.sort_by_key(|known| known.start);
    layer.unmatched_calls.retain(|call| {
        answered[..answered.partition_point(|known| known.start < call.end)].last().is_none_or(|known| known.end <= call.start)
    });
}

/// Each heading's words and level: a family of enumerators keeps its level, a new one goes under the last.
fn title_headings(layer: &mut OpinionLayer) {
    let mut calls = layer.notes.iter().flat_map(|note| note.references.iter().copied()).collect::<Vec<_>>();
    calls.sort_by_key(|call| call.start);
    let coordinates = ScalarText::new(&layer.text);
    layer.headings.sort_by_key(|heading| heading.start);
    let mut ladder = Vec::<String>::new();
    let mut letter = None;
    for heading in &mut layer.headings {
        // A note's call on a title is no word of it.
        let within = &calls[calls.partition_point(|call| call.start <= heading.start)..];
        let words_end = within.iter().take_while(|call| call.start < heading.end).find(|call| call.end + 1 >= heading.end).map_or(heading.end, |call| call.start);
        let printed = coordinates.slice_utf16(heading.start..words_end).map(trim_js).unwrap_or("");
        let title = paged_title(unfused(printed));
        heading.text = letter_spaced(title).unwrap_or_else(|| squeezed(title));
        // "C." after "B." is a letter, not a hundred.
        let family = hierarchy::numbering(printed, letter);
        letter = if matches!(family.as_str(), "AU" | "al") { printed.chars().find(|c| c.is_alphabetic()).and_then(|c| c.to_lowercase().next()) } else { letter };
        heading.level = if family.is_empty() {
            ladder.clear();
            1
        } else if let Some(at) = ladder.iter().position(|seen| *seen == family) {
            ladder.truncate(at + 1);
            at + 1
        } else {
            ladder.push(family);
            ladder.len()
        };
    }
}

/// A heading's words decide last: a mark, a rule, a list's title or a contents line is none.
fn sift_headings(layer: &mut OpinionLayer) {
    for heading in std::mem::take(&mut layer.headings) {
        let range = Some(ScalarRange { start: heading.start, end: heading.end });
        if !heading.text.chars().any(char::is_alphanumeric) {
            continue;
        }
        if text::contents_line(&heading.text) {
            layer.front.push(FrontLine { role: "contents".to_owned(), text: heading.text, range });
        } else if let Some(numbers) = heading.text.starts_with('[').then(|| headnote_numbers(&heading.text)).flatten() {
            layer.headnote_marks.push(HeadnoteMark { numbers, start: heading.start, end: heading.end, page: None });
        } else {
            layer.headings.push(heading);
        }
    }
}

/// What is read from the finished text and paragraphs.
fn read_the_text(layer: &mut OpinionLayer) {
    let searched = layer.paragraphs.iter().map(|paragraph| (paragraph.start, paragraph.end))
        .chain(layer.notes.iter().flat_map(|note| note.ranges.iter().map(|range| (range.start, range.end)))).collect::<Vec<_>>();
    layer.defined_terms = defined_terms(&layer.text, &searched);
    layer.quotes = quotations(&layer.text, &layer.paragraphs);
    layer.instruments = instruments(&layer.text, &layer.paragraphs);
    layer.record_gaps = record_gaps(layer);
    layer.document_kind = document_kind(&layer.text).to_owned();
    // Blocks past the last paragraph, or lines of the foot, may carry the order the paragraphs do not.
    let last_text = layer.paragraphs.iter().map(|paragraph| paragraph.start).max().unwrap_or(0);
    let closing = layer.headings.iter().map(|heading| ScalarRange { start: heading.start, end: heading.end })
        .chain(layer.headnotes.iter().filter(|headnote| headnote.kind == "topics").filter_map(|headnote| headnote.range))
        .chain(layer.front.iter().filter_map(|line| line.range))
        .filter(|range| range.start >= last_text)
        // A slip's head may print its disposition: "AFFIRMED."
        .chain(layer.front.iter().filter(|line| line.role == "cover").filter_map(|line| line.range)).collect::<Vec<_>>();
    let (lines, outcomes) = dispositions(&layer.text, &layer.paragraphs, &layer.parts, &closing);
    layer.front.extend(lines);
    layer.outcomes = outcomes;
    name_dates(&mut layer.front);
}

/// The lead numbering's page of everything found, its pins in every numbering, and each paragraph's part.
fn place_on_pages(layer: &mut OpinionLayer) {
    let of_the_opinion = |page: &&OpinionPage| !page.in_note && page.source != "stray";
    let mut counts = HashMap::<&str, usize>::new();
    for page in layer.pages.iter().filter(of_the_opinion) {
        *counts.entry(page.series.as_str()).or_default() += 1;
    }
    let lead = counts.iter().max_by_key(|(series, count)| (series.starts_with("reporter"), **count, std::cmp::Reverse(**series))).map(|(series, _)| *series);
    let lead_pages = layer.pages.iter().filter(of_the_opinion).filter(|page| Some(page.series.as_str()) == lead).map(|page| (page.start, page.label.clone())).collect::<Vec<_>>();
    let page_at = |offset: usize| lead_pages.partition_point(|page| page.0 <= offset).checked_sub(1).map(|at| lead_pages[at].1.clone());
    let table = pin_table(layer);
    for block in &mut layer.blocks {
        block.pins = pins_of(&table, block.start, block.start + 1);
    }
    for heading in &mut layer.headings {
        heading.page = page_at(heading.start);
        heading.pins = pins_of(&table, heading.start, heading.end);
    }
    for paragraph in &mut layer.paragraphs {
        paragraph.pins = pins_of(&table, paragraph.start, paragraph.end);
        paragraph.page = page_at(paragraph.start);
        paragraph.part = layer.parts.iter().position(|part| part.start <= paragraph.start && paragraph.start < part.end);
    }
    // A note's page is its call's: the notes of a text set at its end are still on the page that calls them.
    for note in &mut layer.notes {
        let call = note.references.first().map_or(note.start, |call| call.start);
        note.page = page_at(call);
        note.pins = pins_of(&table, call, call + 1);
    }
    for mark in &mut layer.headnote_marks {
        mark.page = page_at(mark.start);
    }
    for call in &mut layer.unmatched_calls {
        call.page = page_at(call.start);
    }
    for table in &mut layer.tables {
        table.page = page_at(table.start);
    }
    for term in &mut layer.defined_terms {
        term.page = page_at(term.start);
    }
    for quote in &mut layer.quotes {
        quote.page = page_at(quote.start);
    }
}
