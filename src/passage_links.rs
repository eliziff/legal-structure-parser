//! A passage's link on a source's page: the address the citation engine gives the page at the passage's anchor,
//! with the text fragments the planner gives the passage's block and quotations. These are the links Beaver's
//! legalSourceLinks.ts makes (buildLegalSourcePinpoint, citedPassageLink, and A2AJ Law Web's).

use crate::{DocumentKind, DocumentQuery, DocumentStructure, TextFragmentPlan};
use legal_document_query::source_text_fragment_plan;
use regex::Regex;
use std::sync::OnceLock;
use legal_citations::reference_names::reference_name_ends;
use legal_citations::url::{
    is_pdf_address, law_web_document, passage_locator, publisher_annotates_legal_references,
    publisher_may_repeat_passages, source_url,
};
use serde::{Deserialize, Serialize};

/// A PDF of the source a reader may be sent to instead of its page.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedPdf {
    pub url: String,
    #[serde(default)]
    pub pdf_only: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PassageLinkRequest {
    pub url: String,
    pub anchor: Option<String>,
    pub block_text: String,
    #[serde(default)]
    pub quotes: Vec<String>,
    /// Prefer one directive per HTML block where that paints the same words, each once.
    #[serde(default)]
    pub prefer_independent_html_blocks: bool,
    pub verified_pdf: Option<VerifiedPdf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassageLink {
    pub target: String,
    pub plan: Option<TextFragmentPlan>,
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
            annotated, split, publisher_may_repeat_passages(url));
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
    let kind = if section { DocumentKind::Section } else { DocumentKind::Paragraph };
    let plan = |target: &str| query.range_fragment_plan(structure, kind, target, target, pdf, annotated, section,
        publisher_may_repeat_passages(&url)).filter(|plan| plan.source_safe_complete && !plan.directives.is_empty());
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
