//! A passage's link on a source's page: the address the citation engine gives the page at the passage's anchor,
//! with the text fragments the planner gives the passage's block and quotations. These are the links Beaver's
//! legalSourceLinks.ts makes (buildLegalSourcePinpoint, citedPassageLink, and A2AJ Law Web's).

use legal_structure::{canlii_case_url, canlii_law_url, provision_mark, NodeKind, DocumentQuery, DocumentStructure, TextFragmentPlan};
use legal_document_query::{source_text_fragment_plan, TextFragmentPage};
use regex::Regex;
use std::sync::OnceLock;
use legal_citations::reference_names::reference_name_ends;
use legal_citations::url::{
    is_pdf_address, law_web_document, passage_locator, publisher_annotates_legal_references,
    publisher_glues_section_labels, publisher_may_repeat_passages, source_anchor, source_url,
};
use serde::{Deserialize, Serialize};
use crate::courtlistener_page::{print_page, PrintedPage};
pub use crate::courtlistener_page::{CourtListenerAnchor, CourtListenerCluster, CourtListenerOpinion};

/// A PDF of the source a reader may be sent to instead of its page.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct VerifiedPdf {
    pub url: String,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<bool>", optional))]
    pub pdf_only: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[cfg_attr(feature = "binding-types", ts(optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct PassageLinkRequest {
    pub url: String,
    pub anchor: Option<String>,
    pub block_text: String,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<String>>", optional))]
    pub quotes: Vec<String>,
    /// Prefer one directive per HTML block where that paints the same words, each once.
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<bool>", optional))]
    pub prefer_independent_html_blocks: bool,
    pub verified_pdf: Option<VerifiedPdf>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PassageLink {
    pub target: String,
    pub plan: Option<TextFragmentPlan>,
}

/// A passage of an A2AJ document, for the link it is routed to (`source_link`).
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[cfg_attr(feature = "binding-types", ts(optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct SourceLinkRequest {
    /// The publisher's page A2AJ names for the document, where it names one.
    pub url: Option<String>,
    /// The A2AJ dataset (`SCC`, `LEGISLATION-ON`), the document's citation and its other one.
    pub dataset: String,
    pub citation: String,
    #[serde(default)]
    pub alternate_citation: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<String>", optional))]
    pub language: String,
    /// `cases` or `laws`.
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<String>", optional))]
    pub doc_type: String,
    /// The passage's locator where the citation pinpoints one: its kind (`paragraph`, `section`, `page`) and the label
    /// the document prints (`12`, `5(2)`, `3`).
    pub locator: Option<SourceLocator>,
    pub block_text: String,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<String>>", optional))]
    pub quotes: Vec<String>,
    /// The citation's pinpoints (`par12`, `sec6subsec1`, `sec6(1)(c)`): a passage cited without a quotation is
    /// linked at them, as a citation's pinpoint is (`cited_passage_link`).
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<String>>", optional))]
    pub pinpoints: Vec<String>,
    pub verified_pdf: Option<VerifiedPdf>,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct SourceLocator {
    pub kind: String,
    pub label: String,
}

/// The link a passage of an A2AJ document is routed to, planned against `structure` (the document's text): on the
/// publisher's page A2AJ names where that page prints an anchor for the pinpoint (the anchor is where a link lands
/// when its words do not paint), else on the document's CanLII page; the page that exists where only one does; else
/// on A2AJ Law Web's reader, where the passage plans whole there. Measured 2026-10-18 on 3,904 passages judged on
/// both pages: publisher always 99.08% painted on the words / 99.28% on the words or at the anchor; CanLII always
/// 98.13 / 99.46; this rule 99.15 / 99.54. Only the page linked is planned.
pub fn source_link(query: &DocumentQuery, structure: &DocumentStructure, request: &SourceLinkRequest) -> Option<String> {
    // A pinpoint cited without a quotation: its paragraph's or provision's anchor in the engine's form (`par12`,
    // `sec6subsec1`; a provision's part, "6(1)(c)", is anchored at its provision and painted from `pinpoints`), as the
    // measured routing linked it.
    let locator = request.locator.as_ref().map(|locator| (locator.kind.as_str(), locator.label.as_str()));
    let pinpoint = request.quotes.is_empty().then_some(locator).flatten()
        .and_then(|(kind, label)| match kind {
            "paragraph" => Some(("paragraph", format!("par{label}"))),
            "section" => {
                let provision = label.split('(').next().unwrap_or(label).trim();
                (!provision.is_empty()).then(|| ("section", format!("{}{provision}", provision_mark(Some("section")))))
            }
            _ => None,
        });
    let anchor_on = |page: &str| match &pinpoint {
        Some((kind, engine)) => source_anchor(page, kind, engine),
        None => locator.and_then(|(kind, label)| source_anchor(page, kind, label)),
    };
    let link = |page: &str, verified_pdf: Option<VerifiedPdf>| match &pinpoint {
        Some((kind, engine)) => pinpoint_link(query, structure, page, kind, engine, &request.pinpoints),
        None => passage_link(query, structure, &PassageLinkRequest {
            url: page.to_owned(), anchor: anchor_on(page), block_text: request.block_text.clone(),
            quotes: request.quotes.clone(), prefer_independent_html_blocks: false, verified_pdf,
        }),
    };
    let laws = request.doc_type == "laws";
    let language = if request.language.is_empty() { "en" } else { &request.language };
    let publisher = || request.url.as_deref().and_then(|page| link(page, request.verified_pdf.clone()));
    let canlii = || {
        let page = if laws {
            canlii_law_url(&request.citation, language, true)
        } else {
            let citations = [Some(request.citation.clone()), request.alternate_citation.clone()].into_iter()
                .flatten().collect::<Vec<_>>();
            canlii_case_url(&citations, language, Some(&request.dataset))
        };
        page.as_deref().and_then(|page| link(page, None))
    };
    let routed = if request.url.as_deref().and_then(anchor_on).is_some() {
        publisher().or_else(canlii)
    } else {
        canlii().or_else(publisher)
    };
    routed.map(|link| link.target).or_else(|| law_web_link(query, structure, &request.citation,
        if laws { "laws" } else { "cases" }, &request.block_text, &request.quotes))
}

/// A pinpoint linked on `page` as a citation's pinpoint is (`cited_passage_link`: the cited paragraph or provision,
/// the part its pinpoints name, painted where it plans whole), at the anchor that page prints for it, or its words
/// alone where the page prints none.
fn pinpoint_link(query: &DocumentQuery, structure: &DocumentStructure, page: &str, kind: &str, engine: &str,
    pinpoints: &[String]) -> Option<PassageLink> {
    let page = page.split('#').next().unwrap_or(page);
    let linked = cited_passage_link(query, structure, &format!("{page}#{engine}"), pinpoints)?;
    let (base, fragment) = linked.split_once('#').unwrap_or((&linked, ""));
    let directives = fragment.strip_prefix(engine).unwrap_or("");
    let fragment = format!("{}{directives}", source_anchor(base, kind, engine).unwrap_or_default());
    Some(PassageLink { target: if fragment.is_empty() { base.to_owned() } else { format!("{base}#{fragment}") }, plan: None })
}

fn append_directives(url: &str, directives: &[String]) -> String {
    match (directives.is_empty(), url.contains('#')) {
        (true, _) => url.to_owned(),
        (false, true) => format!("{url}:~:{}", directives.join("&")),
        (false, false) => format!("{url}#:~:{}", directives.join("&")),
    }
}

/// The passage's link on the page at `request.url`: the page at the passage's anchor, its block's or quotations'
/// fragments planned against `structure`; a verified PDF where the page cannot be planned whole and the PDF can.
pub fn passage_link(query: &DocumentQuery, structure: &DocumentStructure, request: &PassageLinkRequest)
    -> Option<PassageLink> {
    let base = source_url(&request.url, request.anchor.as_deref())?;
    if request.block_text.is_empty() {
        return Some(PassageLink { target: base, plan: None });
    }
    let plan = |url: &str, pdf: bool| {
        let annotated = publisher_annotates_legal_references(url);
        if annotated {
            query.read_legal_reference_ends(structure, reference_name_ends);
        }
        let fragments = |split: bool| query.text_fragment_plan(structure, &request.block_text, &request.quotes, pdf,
            annotated, split, publisher_may_repeat_passages(url), publisher_glues_section_labels(url));
        let whole = fragments(false);
        let value = whole.directives.first().and_then(|directive| directive.strip_prefix("text=")).unwrap_or("");
        let simple_range = whole.directives.len() == 1 && value.split(',').count() == 2
            && !value.contains("-,") && !value.contains(",-");
        if !request.prefer_independent_html_blocks || pdf || !simple_range {
            return whole;
        }
        let blocks = fragments(true);
        if blocks.source_safe_complete && blocks.directives.len() >= 2 && blocks.painted_words == whole.painted_words
            && query.text_statistics(structure, &blocks.paint_quotes).iter().all(|found| found.occurrences == 1) {
            blocks
        } else {
            whole
        }
    };
    let verified = request.verified_pdf.as_ref()
        .and_then(|pdf| source_url(&pdf.url, request.anchor.as_deref()).map(|url| (url, pdf.pdf_only)));
    let mut target = base.clone();
    let mut pdf = is_pdf_address(&target);
    if !pdf {
        if let Some((url, true)) = &verified {
            target = url.clone();
            pdf = true;
        }
    }
    let mut selected = plan(&target, pdf);
    if !pdf && !selected.source_safe_complete {
        if let Some((url, _)) = &verified {
            let fallback = plan(url, true);
            if fallback.source_safe_complete || fallback.painted_words > selected.painted_words {
                target = url.clone();
                selected = fallback;
            }
        }
    }
    Some(PassageLink { target: append_directives(&target, &selected.directives), plan: Some(selected) })
}

/// The link citing a passage by its anchor (`…#par12`, `…#sec7`): the paragraph's, or the provision's (the part
/// of it a pinpoint names, else its opening block), fragments added where they plan whole.
pub fn cited_passage_link(query: &DocumentQuery, structure: &DocumentStructure, url: &str, pinpoints: &[String])
    -> Option<String> {
    let url = source_url(url, None)?;
    let Some(locator) = passage_locator(&url, pinpoints) else { return Some(url) };
    let section = locator.kind == "section";
    let (pdf, annotated) = (is_pdf_address(&url), publisher_annotates_legal_references(&url));
    if annotated {
        query.read_legal_reference_ends(structure, reference_name_ends);
    }
    let kind = if section { NodeKind::Section } else { NodeKind::Paragraph };
    let plan = |target: &str| query.range_fragment_plan(structure, kind, target, target, pdf, annotated, section,
        publisher_may_repeat_passages(&url), publisher_glues_section_labels(&url))
        .filter(|plan| plan.source_safe_complete && !plan.directives.is_empty());
    // A part the pinpoint names is painted or nothing is: the provision's opening block in its place would paint the
    // wrong part (measured 2026-10-18, 13 links).
    let planned = match locator.part.as_deref() { Some(part) => plan(part), None => plan(&locator.label) };
    Some(planned.map_or_else(|| url.clone(), |plan| append_directives(&url, &plan.directives)))
}

/// The passage on A2AJ Law Web's reader of the document, planned over the text that reader shows
/// ([`law_web_page_text`]): None where the plan is incomplete there.
pub fn law_web_link(query: &DocumentQuery, structure: &DocumentStructure, citation: &str, doc_type: &str,
    block_text: &str, quotes: &[String]) -> Option<String> {
    let laws = doc_type == "laws";
    let page = query.source_page(|| law_web_page_text(&structure.text, laws, Some(LAW_WEB_FIRST_FETCH)));
    let shown = |text: &str| law_web_page_text(text, laws, None);
    let quotes = quotes.iter().map(|quote| shown(quote)).collect::<Vec<_>>();
    let plan = source_text_fragment_plan(&shown(block_text), page, &quotes);
    if citation.is_empty() || !plan.source_safe_complete || plan.directives.is_empty() {
        return None;
    }
    Some(format!("{}#:~:{}", law_web_document(citation, doc_type), plan.directives.join("&")))
}

/// A case's page on CourtListener, read from its bulk data alone (courtlistener_page.rs prints it as CourtListener's code
/// publishes it): the words the page prints (`text`, one line per block), which of them are page labels and note calls,
/// and the places it can be linked at.
pub struct CourtListenerPage {
    page: TextFragmentPage,
    marks: Vec<(usize, usize)>,
    anchors: Vec<CourtListenerAnchor>,
    article: usize,
    /// The page's letters and digits, lowercase, marks left out; for each byte of them the byte of the text it came from.
    letters: String,
    source: Vec<u32>,
}

/// A word a reader's text glues to the star page before it ("*504light", "*427a" stays a page): the word alone.
fn unglued_star(word: &str) -> &str {
    let Some(rest) = word.strip_prefix('*') else { return word };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let tail = &rest[digits..];
    if digits == 0 || tail.chars().count() <= 1 { word } else { tail }
}

fn percent_decoded(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() + 0 && at + 2 <= bytes.len() - 1 {
            if let Ok(byte) = u8::from_str_radix(&value[at + 1..at + 3], 16) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn star_page(word: &str) -> bool {
    word.strip_prefix('*').is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}

impl CourtListenerPage {
    /// The page of a case from its bulk data.
    pub fn new(cluster: &CourtListenerCluster) -> Self {
        let PrintedPage { text, marks, anchors, article } = print_page(cluster);
        let (mut letters, mut source) = (String::with_capacity(text.len()), Vec::with_capacity(text.len()));
        let mut open = marks.iter().peekable();
        for (at, character) in text.char_indices() {
            while open.peek().is_some_and(|mark| mark.1 <= at) {
                open.next();
            }
            if open.peek().is_some_and(|mark| mark.0 <= at) || !character.is_alphanumeric() {
                continue;
            }
            for lower in character.to_lowercase() {
                letters.push(lower);
                source.resize(letters.len(), at as u32);
            }
        }
        Self { page: TextFragmentPage::new(text), marks, anchors, article, letters, source }
    }

    /// The byte of the text where the opinion tab's article begins: above it the site's menu, sidebar and caption.
    pub fn article_start(&self) -> usize {
        self.article
    }

    pub fn text(&self) -> &str {
        self.page.text()
    }

    pub fn marks(&self) -> &[(usize, usize)] {
        &self.marks
    }

    pub fn anchors(&self) -> &[CourtListenerAnchor] {
        &self.anchors
    }

    /// Where `words` print, read past marks, line ends and word breaks: the bytes of the text from their first to their
    /// last, inside `within` where given.
    fn locate(&self, words: &str, within: Option<(usize, usize)>) -> Option<(usize, usize)> {
        // A paragraph's printed number stands in a reader's text; a page holding that paragraph in another record's
        // printing has none.
        self.locate_words(words, within).or_else(|| {
            let (first, rest) = words.trim_start().split_once(char::is_whitespace)?;
            first.bytes().all(|byte| byte.is_ascii_digit()).then(|| self.locate_words(rest, within)).flatten()
        })
    }

    fn locate_words(&self, words: &str, within: Option<(usize, usize)>) -> Option<(usize, usize)> {
        self.locate_after(words, within.map_or(0, |(start, _)| start), within.map(|(_, end)| end))
    }

    /// Where `words` print first at or after byte `after` of the text, ending by byte `limit` where given.
    fn locate_after(&self, words: &str, after: usize, limit: Option<usize>) -> Option<(usize, usize)> {
        // A reader's text may hold a star page ("stems *76 from"), left out of `letters` as a mark.
        let wanted = words.split_whitespace().filter(|word| !star_page(word)).map(unglued_star).flat_map(str::chars)
            .filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
        if wanted.is_empty() {
            return None;
        }
        let from = self.source.partition_point(|&at| (at as usize) < after);
        let found = from + self.letters[from..].find(&wanted)?;
        let start = self.source[found] as usize;
        let last = self.source[found + wanted.len() - 1] as usize;
        let end = last + self.text()[last..].chars().next().map_or(0, char::len_utf8);
        limit.is_none_or(|limit| end <= limit).then_some((start, end))
    }

    /// Where a quotation prints, as a reader may hold it: whole; else its pieces between elisions ("...", "* * *"), each
    /// after the last; else from its first words to its last, where both print in order no further apart than the
    /// quotation's own length allows (another printing of the same words: other line breaks, a corrected letter between).
    fn printed_spans(&self, quote: &str, within: Option<(usize, usize)>) -> Vec<(usize, usize)> {
        if let Some(span) = self.locate(quote, within).or_else(|| self.locate(quote, None)) {
            return vec![span];
        }
        static ELISION: OnceLock<Regex> = OnceLock::new();
        let elision = ELISION.get_or_init(|| Regex::new(r"\.\s?\.\s?\.|…|\*\s\*\s\*").unwrap());
        let pieces = elision.split(quote).map(str::trim).filter(|piece| piece.split_whitespace().count() >= 3).collect::<Vec<_>>();
        if pieces.len() >= 2 {
            let mut after = within.map_or(0, |(start, _)| start);
            let spans = pieces.iter().map_while(|piece| {
                let span = self.locate_after(piece, after, None)?;
                after = span.1;
                Some(span)
            }).collect::<Vec<_>>();
            if spans.len() == pieces.len() {
                return spans;
            }
        }
        let words = quote.split_whitespace().filter(|word| !star_page(word)).collect::<Vec<_>>();
        if words.len() < 12 {
            return Vec::new();
        }
        let (head, tail) = (words[..6].join(" "), words[words.len() - 6..].join(" "));
        let after = within.map_or(0, |(start, _)| start);
        let Some(first) = self.locate_after(&head, after, None) else { return Vec::new() };
        match self.locate_after(&tail, first.1, None) {
            Some(last) if last.1 - first.0 <= quote.len() * 3 / 2 + 200 => vec![(first.0, last.1)],
            _ => Vec::new(),
        }
    }

    /// Whether a text directive with no prefix finds its first term first inside one of `quotes` (each a slice of the
    /// page's text), by letters and digits as a browser's search folds case and spacing.
    fn starts_inside(&self, directive: &str, quotes: &[String]) -> bool {
        let value = directive.strip_prefix("text=").unwrap_or(directive);
        let mut terms = value.split(',');
        let Some(first) = terms.next() else { return true };
        if first.ends_with('-') {
            return true;
        }
        let decoded = percent_decoded(first);
        let Some(found) = self.locate_after(&decoded, 0, None) else { return true };
        let text = self.text();
        quotes.iter().filter_map(|quote| text.find(quote.as_str()).map(|at| (at, at + quote.len())))
            .any(|(start, end)| start <= found.0 && found.0 < end)
    }

    /// The anchor of `kind` the page prints once, last at or before `words` of `block`: where a link to them lands.
    pub fn anchor_before(&self, block: &str, words: &str, kind: &str) -> Option<&CourtListenerAnchor> {
        let within = self.locate(block, None);
        let (start, _) = self.locate(words, within).or_else(|| self.locate(words, None))?;
        self.anchors.iter().filter(|anchor| anchor.kind == kind && anchor.at <= start).last().filter(|anchor| anchor.unique)
    }
}

/// The passage's link on its CourtListener page at `url`, at `anchor` where given: the page's text is all a plan reads,
/// from the case's bulk data, never from the page itself.
pub fn courtlistener_link(page: &CourtListenerPage, url: &str, anchor: Option<&str>, block_text: &str, quotes: &[String])
    -> Option<PassageLink> {
    let base = source_url(url, anchor)?;
    let text = page.text();
    let block = page.locate(block_text, None);
    // A star page inside the words splits them: a term that crosses it ("to *277 exercise") is matched by no page, so
    // each side is a quotation of its own; so too a run of blanks ("action____").
    let printed = quotes.iter().flat_map(|quote| page.printed_spans(quote, block)).flat_map(|(start, end)| {
        let mut pieces = Vec::new();
        let mut from = start;
        // A mark inside a word ("stat*342ute", "*36to") leaves no word boundary beside it: the partial words go too.
        let word = |c: char| c.is_alphanumeric();
        for &(mark_start, mark_end) in page.marks.iter().filter(|(mark_start, mark_end)| start < *mark_start && *mark_end < end
            && text[*mark_start..*mark_end].starts_with('*')) {
            let mut left = mark_start;
            if text[..mark_start].ends_with(word) {
                left = text[from..mark_start].rfind(char::is_whitespace).map_or(from, |at| from + at);
            }
            pieces.push((from, left));
            from = mark_end;
            if text[mark_end..].starts_with(word) {
                from = text[mark_end..end].find(char::is_whitespace).map_or(end, |at| mark_end + at);
            }
        }
        pieces.push((from, end));
        pieces.into_iter().flat_map(|(start, end)| text[start..end].split("__").map(|piece| piece.trim_matches(|c: char| c == '_' || c.is_whitespace()).to_owned())
            .filter(|piece| piece.chars().any(char::is_alphanumeric)).collect::<Vec<_>>()).collect::<Vec<_>>()
    }).collect::<Vec<_>>();
    if printed.is_empty() {
        return Some(PassageLink { target: base, plan: None });
    }
    let block = block.map_or_else(|| printed[0].clone(), |(start, end)| text[start..end].to_owned());
    // A plain-text opinion wraps its lines with <br>, which no term may cross: a short quotation is its lines'.
    let lined = printed.iter().flat_map(|quote| {
        let lines = quote.split('\n').map(str::to_owned).collect::<Vec<_>>();
        if (2..=3).contains(&lines.len()) { lines } else { vec![quote.clone()] }
    }).collect::<Vec<_>>();
    let mut plan = source_text_fragment_plan(&block, &page.page, &lined);
    // Every directive's first word is found on the page first where the quotation stands, as a browser searches it;
    // a plan of its lines that starts elsewhere ("U.S.C" before the one quoted) gives way to the plan of the whole.
    if lined != printed && !plan.directives.iter().all(|directive| page.starts_inside(directive, &printed)) {
        plan = source_text_fragment_plan(&block, &page.page, &printed);
    }
    Some(PassageLink { target: append_directives(&base, &plan.directives), plan: Some(plan) })
}

/// The characters Law Web's reader asks /fetch for first (document.js: `end_char: offset + CHUNK + 1`, CHUNK
/// 200,000); it shows no more until "load more" is pressed.
const LAW_WEB_FIRST_FETCH: usize = 200_001;

/// The text Law Web's reader shows for `text` (github.com/a2aj-ca/a2aj-law-web, document.js): a case's text as it
/// is; a law's lines drawn by renderLawText, each a block of its own, blank lines dropped, a heading's `#` marks
/// dropped and `*emphasis*` shown without its stars. Only the first `limit` characters where given.
pub fn law_web_page_text(text: &str, laws: bool, limit: Option<usize>) -> String {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    static EMPHASIS: OnceLock<Regex> = OnceLock::new();
    let fetched = match limit {
        Some(limit) => text.char_indices().nth(limit).map_or(text, |(at, _)| &text[..at]),
        None => text,
    };
    if !laws {
        return fetched.to_owned();
    }
    let heading = HEADING.get_or_init(|| Regex::new(r"^#{1,5}\s+(.*)").unwrap());
    let emphasis = EMPHASIS.get_or_init(|| Regex::new(r"\*([^*\n]+)\*").unwrap());
    fetched.split('\n').filter(|line| !line.trim().is_empty()).map(|line| match heading.captures(line) {
        Some(found) => found[1].to_owned(),
        None => emphasis.replace_all(line, "$1").into_owned(),
    }).collect::<Vec<_>>().join("\n")
}
