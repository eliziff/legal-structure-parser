use super::search;
use super::*;
use crate::{javascript_whitespace, utf16_len};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

impl DocumentQuery {
    pub fn text_fragment_plan(
        &self,
        document: &DocumentStructure,
        block_text: &str,
        quotes: &[String],
        pdf: bool,
        publisher_may_annotate_legal_reference: bool,
        split_html_source_blocks: bool,
        page_may_repeat_passages: bool,
        page_glues_labels: bool,
    ) -> TextFragmentPlan {
        let text = self.text(document);
        let source = FragmentText::Document(self, document, &text, page_glues_labels);
        let base = build_fragment_plan(
            block_text,
            source,
            quotes,
            pdf,
            publisher_may_annotate_legal_reference,
            false,
            false,
            page_may_repeat_passages,
        );
        if !split_html_source_blocks {
            return base;
        }
        let blocks = build_fragment_plan(
            block_text,
            source,
            quotes,
            pdf,
            publisher_may_annotate_legal_reference,
            true,
            false,
            page_may_repeat_passages,
        );
        if !blocks.source_safe_complete || crosses_multiple_lettered_breaks(source, &base) {
            base
        } else {
            blocks
        }
    }

    pub fn range_fragment_plan(
        &self,
        document: &DocumentStructure,
        kind: DocumentKind,
        start: &str,
        end: &str,
        pdf: bool,
        publisher_may_annotate_legal_reference: bool,
        first_block_only: bool,
        page_may_repeat_passages: bool,
        page_glues_labels: bool,
    ) -> Option<TextFragmentPlan> {
        let resolve = |locator: &str| {
            let label = self.requested_label(document, kind, locator);
            if label.is_empty() { return None; }
            let position = self.unique_position(document, &label, Some(kind))?;
            // A citation's paragraph is the one printed with its number, never one the structure counted where it
            // read no numbers: the 38th block of prose is not paragraph 38.
            let node = &document.nodes[position.node];
            (kind != DocumentKind::Paragraph || node.marker_range.is_some_and(|range| range.start < range.end))
                .then_some(position)
        };
        let first = resolve(start)?;
        let last = resolve(end)?;
        if document.query_range(&document.nodes[first.node]).start
            > document.query_range(&document.nodes[last.node]).start {
            return None;
        }
        let selected = self.read_range(document, kind, start, end, 0)?;
        let text = self.text(document);
        let source = FragmentText::Document(self, document, &text, page_glues_labels);
        let mut ranges = selected.selected.into_iter()
            .take(if first_block_only { 1 } else { usize::MAX })
            .flat_map(|block| block.block.ranges)
            .map(|mut range| {
                if kind == DocumentKind::Paragraph {
                    range.start = (range.start + source.paragraph_marker_length(range.start)).min(range.end);
                }
                range
            })
            .filter(|range| range.start < range.end)
            .collect::<Vec<_>>();
        ScalarRange::normalize_union(&mut ranges);
        let members = ranges;
        self.searched.store(true, Ordering::Relaxed);
        let words = source.tokens();
        let mut complete = true;
        let mut selections = Vec::new();
        for (quote_index, range) in members.iter().enumerate() {
            let first_word = words.partition_point(|word| word.end <= range.start);
            let after = words.partition_point(|word| word.start < range.end);
            if first_word == after { continue; }
            let last_word = after - 1;
            if words[first_word].start < range.start || words[last_word].end > range.end {
                complete = false;
                continue;
            }
            // The member's words, from the first its printed label leaves.
            let mut selection = trimmed_selection(source, quote_index,
                PhraseSpan { start: range.start, end: range.end, first_word, last_word }, false);
            // A page that prints the number against the word after it leaves no word boundary before that word
            // ("57An application"): the passage starts at the next.
            let first = selection.desired.first_word;
            if source.glues_labels() && selection.clamp_leading && first > 0 && first < selection.desired.last_word
                && source.slice(words[first - 1].start, words[first - 1].end).bytes().all(|byte| byte.is_ascii_digit())
                && source.slice(words[first - 1].end, words[first].start).chars().all(javascript_whitespace)
            {
                selection.desired.first_word += 1;
            }
            selection.desired.start = words[selection.desired.first_word].start;
            selection.desired.end = words[selection.desired.last_word].end;
            selections.push(selection);
        }
        let mut plan = build_selected_fragment_plan(source, &selections, pdf,
            publisher_may_annotate_legal_reference, false, true, complete, false, page_may_repeat_passages);
        // Replay must select these source coordinates, not merely the same
        // amount of text. No emitted directive may bridge an unselected member.
        let required = selections.iter().flat_map(|selection|
            selection.desired.first_word..=selection.desired.last_word).collect::<HashSet<_>>();
        let painted = plan.source_word_intervals.iter().flat_map(|span|
            span.first_word..=span.last_word).collect::<HashSet<_>>();
        plan.source_safe_complete &= required == painted && plan.source_word_intervals.iter().all(|span|
            members.iter().any(|member| member.start <= span.start && span.end <= member.end)
                && words.get(span.first_word).is_some_and(|word| word.start == span.start)
                && words.get(span.last_word).is_some_and(|word| word.end == span.end));
        Some(plan)
    }

}

struct TextSearch<'a> {
    text: ScalarText<'a>,
    index: std::borrow::Cow<'a, search::SearchIndex>,
    /// Its replay index, built on the first plan that replays the text.
    replay: std::borrow::Cow<'a, OnceLock<ReplayIndex>>,
}

impl<'a> TextSearch<'a> {
    fn new(text: &'a str) -> Self {
        let scalar = ScalarText::new(text);
        Self {
            index: std::borrow::Cow::Owned(search::SearchIndex::new(text)),
            text: scalar,
            replay: std::borrow::Cow::Owned(OnceLock::new()),
        }
    }
}

/// A page's text with its word indexes, built once and planned on for every link to it.
pub struct TextFragmentPage {
    text: String,
    index: search::SearchIndex,
    replay: OnceLock<ReplayIndex>,
}

impl TextFragmentPage {
    pub fn new(text: String) -> Self {
        let index = search::SearchIndex::new(&text);
        Self { text, index, replay: OnceLock::new() }
    }

    fn search(&self) -> TextSearch<'_> {
        TextSearch {
            text: ScalarText::new(&self.text),
            index: std::borrow::Cow::Borrowed(&self.index),
            replay: std::borrow::Cow::Borrowed(&self.replay),
        }
    }
}

#[derive(Clone, Copy)]
enum FragmentText<'a> {
    /// A document's text, on a page that prints a section's number against its first word ("57An application") or not.
    Document(&'a DocumentQuery, &'a DocumentStructure, &'a ScalarText<'a>, bool),
    Text(&'a TextSearch<'a>),
}

impl<'a> FragmentText<'a> {
    /// Whether a legal reference's name ends after `from` and by `to` (DocumentQuery::read_legal_reference_ends).
    fn legal_reference_ends_within(self, from: usize, to: usize) -> bool {
        match self {
            Self::Document(query, ..) => query.legal_reference_ends.get().is_some_and(|ends| {
                ends.get(ends.partition_point(|&end| end <= from)).is_some_and(|&end| end <= to)
            }),
            Self::Text(_) => false,
        }
    }

    /// Whether the page prints a section's number against its first word.
    fn glues_labels(self) -> bool {
        match self {
            Self::Document(.., glued) => glued,
            Self::Text(_) => false,
        }
    }

    fn tokens(self) -> &'a [search::WordOffset] {
        match self {
            Self::Document(query, document, text, _) => &query.index_with_text(document, text).tokens,
            Self::Text(query) => &query.index.tokens,
        }
    }

    fn phrase_spans(self, words: &[String], options: PhraseOptions) -> Vec<PhraseSpan> {
        match self {
            Self::Document(query, document, text, _) => {
                query.phrase_spans_with_text(document, words, options, text)
            }
            Self::Text(query) => {
                search::indexed_phrase_spans(&query.index, &query.text, words, options)
            }
        }
    }

    fn slice(self, start: usize, end: usize) -> &'a str {
        self.try_slice(start, end).unwrap_or_default()
    }

    /// The whole text.
    fn value(self) -> &'a str {
        match self {
            Self::Document(_, _, text, _) => text.value,
            Self::Text(query) => query.text.value,
        }
    }

    fn try_slice(self, start: usize, end: usize) -> Option<&'a str> {
        match self {
            Self::Document(_, _, text, _) => text.slice_utf16(start..end),
            Self::Text(query) => query.text.slice_utf16(start..end),
        }
    }

    fn utf16_len(self) -> usize {
        match self {
            Self::Document(_, _, text, _) => text.utf16_len(),
            Self::Text(query) => query.text.utf16_len(),
        }
    }

    fn paragraph_marker_length(self, start: usize) -> usize {
        let Self::Document(_, document, ..) = self else {
            return 0;
        };
        document
            .nodes
            .iter()
            .filter_map(|node| {
                // Marker offsets belong to source text, not a separate rendered projection.
                if projected_kind(node) != Some(DocumentKind::Paragraph)
                    || document.query_range(node) != node.range
                {
                    return None;
                }
                let content = node.content_start.or_else(|| {
                    // Some inferred paragraphs retain the label and range only.
                    // Match that known label at the node boundary, never an arbitrary number.
                    let label = node.label.as_deref()?.strip_prefix("par")?;
                    let tail = self
                        .slice(node.range.start, node.range.end)
                        .strip_prefix(label)?;
                    let spaces = tail
                        .chars()
                        .take_while(|c| javascript_whitespace(*c))
                        .map(char::len_utf16)
                        .sum::<usize>();
                    (spaces > 0).then(|| node.range.start + utf16_len(label) + spaces)
                })?;
                let marker = node.marker_range.unwrap_or(ScalarRange {
                    start: node.range.start,
                    end: content,
                });
                (marker.start <= start
                    && start < marker.end
                    && marker.end <= content
                    && content < node.range.end)
                    .then(|| content - start)
            })
            .max()
            .unwrap_or(0)
    }
}

fn normalize_blank_whitespace(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut separating = false;
    for character in js_trim(value).chars() {
        if matches!(
            character,
            ' ' | '\t' | '\r' | '\n' | '\u{000c}' | '\u{000b}'
        ) {
            separating = !normalized.is_empty();
        } else {
            if separating {
                normalized.push(' ');
            }
            normalized.push(character);
            separating = false;
        }
    }
    normalized
}

fn leading_label_length(value: &str) -> usize {
    static LABELS: OnceLock<Vec<Regex>> = OnceLock::new();
    let trimmed_end = value.trim_end_matches(javascript_whitespace);
    LABELS
        .get_or_init(|| {
            [
                r"(?u)^\[\s*[0-9]{1,4}\s*\]\s*",
                r"(?u)^[0-9]{1,4}\]\s*",
                r"(?u)^[0-9]{1,4}(?:\.[0-9]{1,4})*\s*(?:\(\s*[A-Za-z0-9]{1,5}\s*\)\s*)+(?:[.;:]\s*(?:\(\s*[A-Za-z0-9]{1,5}\s*\)\s*)*)?",
                r"(?u)^\(\s*[A-Za-z0-9]{1,5}\s*\)\s*",
            ]
            .map(|pattern| Regex::new(&pattern.replace(r"\s", JS_WS)).expect("literal label regex"))
            .into()
        })
        .iter()
        .filter_map(|pattern| pattern.find(value))
        .find(|found| found.end() < trimmed_end.len())
        .map_or_else(|| provision_number_length(value), |found| utf16_len(&value[..found.end()]))
}

/// A provision's bare number opening its text ("49.2 Where", "6A The"), which a statute's page prints apart from the
/// words, as it prints "[85]" apart from a paragraph's.
fn provision_number_length(value: &str) -> usize {
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    js_regex(r"(?u)^[0-9]{1,4}(?:\.[0-9]{1,4})*[A-Z]?\s+", &NUMBER).find(value)
        .filter(|found| value[found.end()..].chars().next()
            .is_some_and(|next| next.is_uppercase() || matches!(next, '(' | '\u{201C}' | '"')))
        .map_or(0, |found| utf16_len(&value[..found.end()]))
}

fn markdown_label_length(value: &str) -> usize {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    js_regex(
        r"(?u)^(?:(?:\*{1,3}|_{1,3})\s*)?[0-9]{1,4}(?:\.[0-9]{1,4})*\s*(?:\*{1,3}|_{1,3})\s*",
        &LABEL,
    )
    .find(value)
    .map_or(0, |found| utf16_len(&value[..found.end()]))
}

/// A printed label at the start of `value`, markdown's bold number ("**78** The Minister", a federal act's as A2AJ
/// holds it; a page prints the number plain) counted in every mode: measured 2026-10-18, a bare "78" left in a
/// passage became a range's start and painted a history note.
fn leading_label_length_with_mode(value: &str, _markdown: bool) -> usize {
    leading_label_length(value).max(markdown_label_length(value))
}

fn word_at_or_after(text: FragmentText<'_>, offset: usize, from: usize) -> Option<usize> {
    text.tokens()
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, word)| word.end > offset)
        .map(|(index, _)| index)
}

fn word_at_or_before(text: FragmentText<'_>, offset: usize, from: usize) -> Option<usize> {
    let words = text.tokens();
    (0..=from.min(words.len().saturating_sub(1)))
        .rev()
        .find(|index| words[*index].start < offset)
}

fn adjust_span_edges_with_options(
    text: FragmentText<'_>,
    original: PhraseSpan,
    trim_leading: bool,
    markdown_labels: bool,
) -> PhraseSpan {
    static TRAILING_ARTIFACT: OnceLock<Regex> = OnceLock::new();
    static LEADING_ATTACHED_ORDER: OnceLock<Regex> = OnceLock::new();
    static LEADING_NUMBERED_ITEM: OnceLock<Regex> = OnceLock::new();
    let mut start = original.start;
    let mut end = original.end;
    if trim_leading {
        let value = text.slice(start, end);
        let leading = [
            js_regex(
                r"(?u)^(\[\s*\d{1,4}\s*\])AND\s+CONSIDERING\b",
                &LEADING_ATTACHED_ORDER,
            ),
            js_regex(r"(?u)^(\d{1,4}\s+[–—]\s+)\p{Lu}", &LEADING_NUMBERED_ITEM),
        ]
        .into_iter()
        .find_map(|pattern| pattern.captures(value))
        .and_then(|captures| captures.get(1));
        if let Some(leading) = leading {
            start += utf16_len(leading.as_str());
        }
        loop {
            let length = leading_label_length_with_mode(text.slice(start, end), markdown_labels);
            if length == 0 {
                break;
            }
            start += length;
        }
    }
    loop {
        let Some(artifact) = js_regex(
            r"(?u)\s*[.,;:]?\[\s*[0-9]{1,4}(?:\s*[-–—,;]\s*[0-9]{1,4})+\s*\]\s*$",
            &TRAILING_ARTIFACT,
        )
        .find(text.slice(start, end)) else {
            break;
        };
        end -= utf16_len(artifact.as_str());
        if end <= start {
            return original;
        }
    }
    if start == original.start && end == original.end {
        return original;
    }
    let (Some(first_word), Some(last_word)) = (
        word_at_or_after(text, start, original.first_word),
        word_at_or_before(text, end, original.last_word),
    ) else {
        return original;
    };
    if last_word < first_word {
        original
    } else {
        PhraseSpan {
            start,
            end,
            first_word,
            last_word,
        }
    }
}

fn fragment_words(value: &str) -> Vec<String> {
    search::lowercase_words(value)
}

fn choose_source_span(text: FragmentText<'_>, quote: &str, words: &[String]) -> Option<PhraseSpan> {
    static BETWEEN_QUOTES: OnceLock<Regex> = OnceLock::new();
    static OPENING_QUOTE: OnceLock<Regex> = OnceLock::new();
    static CLOSING_QUOTE: OnceLock<Regex> = OnceLock::new();
    // A span runs from its first word to its last: the quotation's own brackets and closing marks stay out of the
    // terms (a browser matches at word boundaries either way, and a page prints "(Emphasis added)" where the
    // document holds "[Emphasis added.]"; measured 2026-10-18).
    let mut spans = text.phrase_spans(words, PhraseOptions::default());
    if spans.is_empty() {
        let separated = quote
            .chars()
            .map(|character| {
                if matches!(character, '[' | ']' | '"') {
                    ' '
                } else {
                    character
                }
            })
            .collect::<String>();
        spans = text.phrase_spans(&fragment_words(&separated), PhraseOptions::default());
    }
    if spans.len() == 1 {
        return spans.pop();
    }
    if spans.is_empty() {
        return None;
    }
    let rendered = spans
        .iter()
        .map(|span| normalize_blank_whitespace(text.slice(span.start, span.end)))
        .collect::<Vec<_>>();
    let straight_to_curly = js_regex(r#"(\S)\"(\S)"#, &BETWEEN_QUOTES)
        .replace_all(quote, "$1“$2")
        .into_owned();
    let straight_to_curly = js_regex(r#"\"(\S)"#, &OPENING_QUOTE)
        .replace_all(&straight_to_curly, "“$1")
        .into_owned();
    let straight_to_curly = js_regex(r#"(\S)\""#, &CLOSING_QUOTE)
        .replace_all(&straight_to_curly, "$1”")
        .into_owned();
    let wanted = [
        normalize_blank_whitespace(
            js_trim(quote).trim_matches(|character| matches!(character, '"' | '\'' | '“' | '”')),
        ),
        quote_text(quote),
        normalize_blank_whitespace(&straight_to_curly),
    ];
    let mut folded_rendered = None::<Vec<String>>;
    for wanted in wanted {
        for folded in [false, true] {
            let folded_wanted = folded.then(|| wanted.to_lowercase());
            let mut matches = spans.iter().enumerate().filter(|(index, _)| {
                if let Some(wanted) = &folded_wanted {
                    folded_rendered.get_or_insert_with(|| {
                        rendered.iter().map(|value| value.to_lowercase()).collect()
                    })[*index]
                        == *wanted
                } else {
                    rendered[*index] == wanted
                }
            });
            let first = matches.next();
            if first.is_some() && matches.next().is_none() {
                return first.map(|(_, span)| span.clone());
            }
        }
    }
    spans.into_iter().next()
}

fn encode_text_fragment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(*byte >> 4) as usize] as char);
            encoded.push(HEX[(*byte & 0xf) as usize] as char);
        }
    }
    encoded
}

fn encoded_fragment(value: &str) -> String {
    encode_text_fragment(&normalize_blank_whitespace(value))
}

fn encoded_context(value: &str, prefix: bool) -> String {
    if value.is_empty() {
        String::new()
    } else if prefix {
        format!("{}-,", encoded_fragment(value))
    } else {
        format!(",-{}", encoded_fragment(value))
    }
}

/// A term without the punctuation outside its words on the sides no context meets: a browser matches at word
/// boundaries, so the marks add nothing there, and a page prints "(Emphasis added)" where the document holds
/// "[Emphasis added.]" (measured 2026-10-18: 85 links painted nothing for an edge mark). A context must meet the
/// term with whitespace alone between, so the side it meets keeps its marks.
fn shed_outer_punctuation(value: &str, leading: bool, trailing: bool) -> String {
    let word = |character: char| character.is_alphanumeric();
    let mut shed = value;
    if leading {
        shed = shed.trim_start_matches(|character: char| !word(character));
    }
    if trailing {
        shed = shed.trim_end_matches(|character: char| !word(character));
    }
    if shed.is_empty() { value.to_owned() } else { shed.to_owned() }
}

fn text_directive(target: &str, prefix: &str, suffix: &str) -> String {
    let target = encoded_fragment(target);
    let prefix = encoded_context(prefix, true);
    let suffix = encoded_context(suffix, false);
    format!("text={prefix}{target}{suffix}")
}

fn text_range_directive_with_context(start: &str, end: &str, prefix: &str, suffix: &str) -> String {
    let prefix = encoded_context(prefix, true);
    let suffix = encoded_context(suffix, false);
    format!(
        "text={prefix}{},{}{suffix}",
        encoded_fragment(start),
        encoded_fragment(end)
    )
}

fn browser_key(value: &str) -> String {
    // ASCII lowercases to ASCII, has no special letters or decompositions and no combining
    // marks, so the general path below returns exactly its ASCII lowercase.
    if value.is_ascii() {
        return value.to_ascii_lowercase();
    }
    let mut expanded = String::new();
    for character in value.chars().flat_map(char::to_lowercase) {
        match character {
            'æ' => expanded.push_str("ae"),
            'œ' => expanded.push_str("oe"),
            'ø' => expanded.push('o'),
            'ł' | 'ŀ' => expanded.push('l'),
            'ð' | 'đ' => expanded.push('d'),
            'ħ' => expanded.push('h'),
            'ß' => expanded.push_str("ss"),
            _ => expanded.push(character),
        }
    }
    expanded
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .collect()
}

struct BrowserSpelledTerm {
    /// Which of a replay's spelled terms this is (BrowserReplay::spelled_term), naming it in the replay's memos.
    id: usize,
    key: String,
    words: Vec<String>,
    leading_punctuation: bool,
    trailing_punctuation: bool,
}

fn browser_spelled_term_with_mode(
    value: &str,
    preserve_punctuation_spacing: bool,
) -> Option<BrowserSpelledTerm> {
    let text = fragment_spelling_with_mode(value, preserve_punctuation_spacing);
    let mut matches = search::word_regex().find_iter(&text).peekable();
    let leading_punctuation = matches.peek()?.start() > 0;
    let mut last_end = 0;
    let words = matches
        .map(|found| {
            last_end = found.end();
            browser_key(&found.as_str().to_lowercase())
        })
        .collect();
    Some(BrowserSpelledTerm {
        id: 0,
        leading_punctuation,
        trailing_punctuation: last_end < text.len(),
        key: browser_key(&text),
        words,
    })
}

#[derive(Clone, Copy, Default)]
struct ExactDirectiveReplay {
    first: Option<PhraseSpan>,
    count: usize,
}

/// What each place an end term can begin (starts[i]) is as a replay's end: unread, no end, or the end it is; and from
/// each, the next occurrence that may be one (a run of no ends is skipped at once).
#[derive(Default)]
struct ValidEnds {
    starts: Vec<usize>,
    ends: Vec<Option<Option<PhraseSpan>>>,
    next: Vec<usize>,
}

impl ValidEnds {
    /// The first occurrence from `i` not known to be no end (`i` itself where it is not), shortening the way there.
    fn next_from(&mut self, mut i: usize) -> usize {
        let start = i;
        while self.next[i] != i {
            i = self.next[i];
        }
        let mut at = start;
        while self.next[at] != at {
            let following = self.next[at];
            self.next[at] = i;
            at = following;
        }
        i
    }
}

/// The document's words as the browser keys them, and where each occurs. They depend only on
/// the document's text, so a document's query builds them once for every plan it makes.
#[derive(Clone)]
pub(super) struct ReplayIndex {
    words: Vec<String>,
    postings: HashMap<u64, Vec<usize>>,
}

impl ReplayIndex {
    fn new(document: FragmentText<'_>) -> Self {
        let tokens = document.tokens();
        let mut postings = HashMap::<u64, Vec<usize>>::new();
        let mut words = Vec::with_capacity(tokens.len());
        // The words come in order: a cursor walks the text from one word's offsets to the next's (UTF-16 to bytes),
        // giving each the text document.slice gives it, nothing where an offset falls inside a character.
        let text = document.value();
        let (mut byte, mut utf16) = (0usize, 0usize);
        let mut at = |target: usize| -> Option<usize> {
            if target < utf16 {
                (byte, utf16) = (0, 0);
            }
            while utf16 < target {
                let character = text[byte..].chars().next()?;
                utf16 += character.len_utf16();
                byte += character.len_utf8();
            }
            (utf16 == target).then_some(byte)
        };
        for (index, token) in tokens.iter().enumerate() {
            let literal = match (at(token.start), at(token.end)) {
                (Some(start), Some(end)) if start <= end => &text[start..end],
                _ => "",
            };
            // An ASCII word's key is its ASCII lowercase (browser_key), made without a second copy.
            let word = if literal.is_ascii() {
                literal.to_ascii_lowercase()
            } else {
                browser_key(&literal.to_lowercase())
            };
            postings
                .entry(search::word_hash(&word))
                .or_default()
                .push(index);
            words.push(word);
        }
        Self { words, postings }
    }
}

struct BrowserReplay<'a> {
    document: FragmentText<'a>,
    preserve_punctuation_spacing: bool,
    index: std::borrow::Cow<'a, ReplayIndex>,
    /// Spelling a term runs several regexes; a plan asks for the same few hundred
    /// terms tens of thousands of times, so each is spelled once.
    spelled: std::cell::RefCell<HashMap<String, Option<Rc<BrowserSpelledTerm>>>>,
    /// The ends found for an end term and the suffix after it (see first_valid_end), whichever start they follow:
    /// each occurrence is read once for every replay of the plan.
    ends: std::cell::RefCell<HashMap<(usize, String, bool), ValidEnds>>,
    /// Where each term can begin (term_starts) and what it matches at a word (spelled_term_bounds_at): a range plan
    /// replays every pairing of its candidate starts and ends, each asking again where the same terms match.
    starts: std::cell::RefCell<HashMap<usize, Rc<[usize]>>>,
    bounds: std::cell::RefCell<HashMap<(usize, usize, bool), Option<(PhraseSpan, usize, usize)>>>,
    /// Each directive's replay, by its terms (the start's, the end's or none, the prefix's and the suffix's, each
    /// usize::MAX where absent): a plan asks again for the replays its earlier candidates made.
    exacts: std::cell::RefCell<HashMap<(usize, usize, usize, bool), ExactDirectiveReplay>>,
    ranges: std::cell::RefCell<HashMap<(usize, usize, usize, usize, bool), Option<PhraseSpan>>>,
}

impl<'a> BrowserReplay<'a> {
    fn new(document: FragmentText<'a>, preserve_punctuation_spacing: bool) -> Self {
        let index = match document {
            FragmentText::Document(query, ..) => {
                std::borrow::Cow::Borrowed(query.replay.get_or_init(|| ReplayIndex::new(document)))
            }
            FragmentText::Text(search) => {
                std::borrow::Cow::Borrowed(search.replay.get_or_init(|| ReplayIndex::new(document)))
            }
        };
        Self {
            document,
            preserve_punctuation_spacing,
            index,
            spelled: std::cell::RefCell::new(HashMap::new()),
            ends: std::cell::RefCell::new(HashMap::new()),
            starts: std::cell::RefCell::new(HashMap::new()),
            bounds: std::cell::RefCell::new(HashMap::new()),
            exacts: std::cell::RefCell::new(HashMap::new()),
            ranges: std::cell::RefCell::new(HashMap::new()),
        }
    }

    fn spelled_term(&self, value: &str) -> Option<Rc<BrowserSpelledTerm>> {
        if let Some(term) = self.spelled.borrow().get(value) {
            return term.clone();
        }
        let id = self.spelled.borrow().len();
        let term = browser_spelled_term_with_mode(value, self.preserve_punctuation_spacing)
            .map(|term| Rc::new(BrowserSpelledTerm { id, ..term }));
        self.spelled
            .borrow_mut()
            .insert(value.to_owned(), term.clone());
        term
    }

    /// A context's term id for the replay memos: usize::MAX where there is no context, None where it spells no term.
    fn context_id(&self, value: &str) -> Option<usize> {
        if value.is_empty() {
            return Some(usize::MAX);
        }
        self.spelled_term(value).map(|term| term.id)
    }

    fn fragment_spelling(&self, value: &str) -> String {
        fragment_spelling_with_mode(value, self.preserve_punctuation_spacing)
    }

    fn literal_word_is(&self, index: usize, wanted: &str) -> bool {
        self.document.tokens().get(index).is_some_and(|word| {
            self.document
                .slice(word.start, word.end)
                .eq_ignore_ascii_case(wanted)
        })
    }

    fn term_at(
        &self,
        wanted: &[String],
        first_word: usize,
        allow_across_lines: bool,
    ) -> Option<PhraseSpan> {
        if wanted.is_empty() || first_word + wanted.len() > self.index.words.len() {
            return None;
        }
        if self.index.words[first_word..first_word + wanted.len()] != *wanted {
            return None;
        }
        let last_word = first_word + wanted.len() - 1;
        let source = self.document.tokens();
        if !allow_across_lines
            && self
                .document
                .slice(source[first_word].start, source[last_word].end)
                .chars()
                .any(|character| matches!(character, '\r' | '\n'))
        {
            return None;
        }
        Some(PhraseSpan {
            start: source[first_word].start,
            end: source[last_word].end,
            first_word,
            last_word,
        })
    }

    fn spelled_term_at(
        &self,
        term: &BrowserSpelledTerm,
        first_word: usize,
        allow_across_lines: bool,
    ) -> Option<PhraseSpan> {
        self.spelled_term_bounds_at(term, first_word, allow_across_lines)
            .map(|(matched, _, _)| matched)
    }

    /// Chromium finds a context term only where white space alone parts it from the text it frames.
    /// Whether only whitespace, or markdown's emphasis marks (which a page prints as nothing: "**1** This"), lies
    /// between `end` and `start`.
    fn adjoins(&self, end: usize, start: usize) -> bool {
        end <= start
            && self
                .document
                .slice(end, start)
                .chars()
                .all(|character| character.is_whitespace() || matches!(character, '*' | '_' | '`'))
    }

    /// A term's words from `first_word`, with the start and end of the text it matches there,
    /// its opening and closing punctuation included.
    fn spelled_term_bounds_at(
        &self,
        term: &BrowserSpelledTerm,
        first_word: usize,
        allow_across_lines: bool,
    ) -> Option<(PhraseSpan, usize, usize)> {
        let key = (term.id, first_word, allow_across_lines);
        if let Some(found) = self.bounds.borrow().get(&key) {
            return *found;
        }
        let found = self.read_spelled_term_bounds_at(term, first_word, allow_across_lines);
        self.bounds.borrow_mut().insert(key, found);
        found
    }

    fn read_spelled_term_bounds_at(
        &self,
        term: &BrowserSpelledTerm,
        first_word: usize,
        allow_across_lines: bool,
    ) -> Option<(PhraseSpan, usize, usize)> {
        let matched = self.term_at(&term.words, first_word, allow_across_lines)?;
        let source = self.document.tokens();
        let word_start = source[first_word].start;
        let word_end = source[matched.last_word].end;
        let first_start = if term.leading_punctuation {
            first_word
                .checked_sub(1)
                .and_then(|previous| source.get(previous))
                .map_or(0, |word| word.end)
        } else {
            word_start
        };
        let last_end = if term.trailing_punctuation {
            source
                .get(matched.last_word + 1)
                .map_or(self.document.utf16_len(), |word| word.start)
        } else {
            word_end
        };
        for start in (first_start..=word_start).rev() {
            for end in word_end..=last_end {
                let rendered = self.fragment_spelling(self.document.slice(start, end));
                if browser_key(&rendered).eq(&term.key) {
                    return Some((matched, start, end));
                }
            }
        }
        None
    }

    fn replay_exact(
        &self,
        target_term: &BrowserSpelledTerm,
        prefix: &str,
        suffix: &str,
        allow_across_lines: bool,
    ) -> ExactDirectiveReplay {
        let (Some(prefix_id), Some(suffix_id)) = (self.context_id(prefix), self.context_id(suffix)) else {
            return ExactDirectiveReplay::default();
        };
        let key = (target_term.id, prefix_id, suffix_id, allow_across_lines);
        if let Some(found) = self.exacts.borrow().get(&key) {
            return *found;
        }
        let found = self.read_replay_exact(target_term, prefix, suffix, allow_across_lines);
        self.exacts.borrow_mut().insert(key, found);
        found
    }

    fn read_replay_exact(
        &self,
        target_term: &BrowserSpelledTerm,
        prefix: &str,
        suffix: &str,
        allow_across_lines: bool,
    ) -> ExactDirectiveReplay {
        let prefix_term = (!prefix.is_empty()).then(|| self.spelled_term(prefix));
        let suffix_term = (!suffix.is_empty()).then(|| self.spelled_term(suffix));
        if prefix_term.as_ref().is_some_and(Option::is_none)
            || suffix_term.as_ref().is_some_and(Option::is_none)
        {
            return ExactDirectiveReplay::default();
        }
        let prefix_term = prefix_term.flatten();
        let suffix_term = suffix_term.flatten();
        let first_term = prefix_term.as_deref().unwrap_or(target_term);
        let candidates = self.term_starts(first_term);
        // A prefix's occurrences that matter are those the target follows: where the target's first word is rarer,
        // they are read from the target's occurrences, the prefix's words before each.
        let starts = match prefix_term.as_ref() {
            Some(prefix) => {
                let targets = self.term_starts(target_term);
                if targets.len() < candidates.len() {
                    targets.iter().filter_map(|target| target.checked_sub(prefix.words.len())).collect::<Rc<[usize]>>()
                } else {
                    candidates
                }
            }
            None => candidates,
        };
        let mut replay = ExactDirectiveReplay::default();
        for &first_word in starts.iter() {
            let prefix_match = prefix_term.as_ref().and_then(|prefix| {
                self.spelled_term_bounds_at(prefix, first_word, allow_across_lines)
            });
            if prefix_term.is_some() && prefix_match.is_none() {
                continue;
            }
            let target_first =
                prefix_match.map_or(first_word, |(matched, _, _)| matched.last_word + 1);
            let Some((target_match, target_start, target_end)) =
                self.spelled_term_bounds_at(target_term, target_first, allow_across_lines)
            else {
                continue;
            };
            if prefix_match.is_some_and(|(_, _, end)| !self.adjoins(end, target_start))
                || suffix_term.as_ref().is_some_and(|suffix| {
                    self.spelled_term_bounds_at(suffix, target_match.last_word + 1, allow_across_lines)
                        .is_none_or(|(_, start, _)| !self.adjoins(target_end, start))
                })
            {
                continue;
            }
            replay.first.get_or_insert(target_match);
            replay.count += 1;
            if replay.count == 2 {
                break;
            }
        }
        replay
    }

    /// Where a term can begin, in order: where its rarest word occurs, less that word's place in the term (a term
    /// matches only where every one of its words does).
    fn term_starts(&self, term: &BrowserSpelledTerm) -> Rc<[usize]> {
        if let Some(starts) = self.starts.borrow().get(&term.id) {
            return starts.clone();
        }
        let (place, occurrences) = term.words.iter().enumerate()
            .map(|(place, word)| (place, self.index.postings.get(&search::word_hash(word)).map_or(&[][..], Vec::as_slice)))
            .min_by_key(|(_, occurrences)| occurrences.len())
            .unwrap_or((0, &[]));
        let starts = occurrences.iter().filter_map(|word| word.checked_sub(place)).collect::<Rc<[usize]>>();
        self.starts.borrow_mut().insert(term.id, starts.clone());
        starts
    }

    fn first_spelled_term_from(
        &self,
        term: &BrowserSpelledTerm,
        from: usize,
        allow_across_lines: bool,
    ) -> Option<PhraseSpan> {
        let starts = self.term_starts(term);
        starts[starts.partition_point(|word| *word < from)..]
            .iter()
            .copied()
            .find_map(|word| self.spelled_term_at(term, word, allow_across_lines))
    }

    /// The first occurrence of `end_term` from word `from` whose suffix (when given) adjoins it: what walking the
    /// occurrences from `from` finds, each occurrence read once for every replay of a plan (`ends`).
    fn first_valid_end(
        &self,
        ends: &mut ValidEnds,
        end_term: &BrowserSpelledTerm,
        suffix_term: Option<&BrowserSpelledTerm>,
        from: usize,
        allow_across_lines: bool,
    ) -> Option<PhraseSpan> {
        if ends.next.is_empty() {
            ends.starts = self.term_starts(end_term).to_vec();
            ends.ends = vec![None; ends.starts.len()];
            ends.next = (0..=ends.starts.len()).collect();
        }
        let starts = std::mem::take(&mut ends.starts);
        let mut i = ends.next_from(starts.partition_point(|word| *word < from));
        let found = loop {
            if i >= starts.len() {
                break None;
            }
            let end = *ends.ends[i].get_or_insert_with(|| {
                let last = self.spelled_term_at(end_term, starts[i], allow_across_lines)?;
                let suffix_adjoins = suffix_term.is_none_or(|suffix| {
                    let last_end = self
                        .spelled_term_bounds_at(end_term, last.first_word, allow_across_lines)
                        .map_or(last.end, |(_, _, end)| end);
                    self.spelled_term_bounds_at(suffix, last.last_word + 1, allow_across_lines)
                        .is_some_and(|(_, start, _)| self.adjoins(last_end, start))
                });
                suffix_adjoins.then_some(last)
            });
            if end.is_some() {
                break end;
            }
            ends.next[i] = i + 1;
            i = ends.next_from(i + 1);
        };
        ends.starts = starts;
        found
    }

    /// Chromium resolves the first start that has a compatible subsequent end.
    /// It does not require either endpoint to be independently unique.
    fn replay_range(
        &self,
        start_term: &BrowserSpelledTerm,
        end_term: &BrowserSpelledTerm,
        prefix: &str,
        suffix: &str,
        allow_across_lines: bool,
    ) -> Option<PhraseSpan> {
        let key = (start_term.id, end_term.id, self.context_id(prefix)?, self.context_id(suffix)?, allow_across_lines);
        if let Some(found) = self.ranges.borrow().get(&key) {
            return *found;
        }
        let found = self.read_replay_range(start_term, end_term, prefix, suffix, allow_across_lines);
        self.ranges.borrow_mut().insert(key, found);
        found
    }

    fn read_replay_range(
        &self,
        start_term: &BrowserSpelledTerm,
        end_term: &BrowserSpelledTerm,
        prefix: &str,
        suffix: &str,
        allow_across_lines: bool,
    ) -> Option<PhraseSpan> {
        let prefix_term = (!prefix.is_empty()).then(|| self.spelled_term(prefix));
        let suffix_term = (!suffix.is_empty()).then(|| self.spelled_term(suffix));
        if prefix_term.as_ref().is_some_and(Option::is_none)
            || suffix_term.as_ref().is_some_and(Option::is_none)
        {
            return None;
        }
        let prefix_term = prefix_term.flatten();
        let suffix_term = suffix_term.flatten();
        let mut kept_ends = self.ends.borrow_mut();
        let ends = kept_ends.entry((end_term.id, suffix.to_owned(), allow_across_lines)).or_default();
        // A context (the prefix where there is one, else the start) in the order the browser meets it: Some(the
        // replay's answer) where it settles the replay, None to read on.
        let mut consider = |context: PhraseSpan| -> Option<Option<PhraseSpan>> {
            let first = if let Some(prefix) = prefix_term.as_ref() {
                let context_end = self
                    .spelled_term_bounds_at(prefix, context.first_word, allow_across_lines)
                    .map_or(context.end, |(_, _, end)| end);
                self.spelled_term_bounds_at(start_term, context.last_word + 1, allow_across_lines)
                    .filter(|(_, start, _)| self.adjoins(context_end, *start))
                    .map(|(matched, _, _)| matched)
            } else {
                Some(context)
            };
            let first = first?;
            if first.last_word + 1 >= self.index.words.len() {
                return None;
            }
            // A later start ends later still: where no end follows this one, none follows any.
            let last = self.first_valid_end(
                ends,
                end_term,
                suffix_term.as_deref(),
                first.last_word + 1,
                allow_across_lines,
            );
            Some(last.map(|last| PhraseSpan {
                start: self.document.tokens()[first.first_word].start,
                end: self.document.tokens()[last.last_word].end,
                first_word: first.first_word,
                last_word: last.last_word,
            }))
        };
        // A prefix's contexts that matter are those the start follows: where the start's first word is rarer than
        // the prefix's, they are read from the start's occurrences, the prefix's words before each.
        if let Some(prefix) = prefix_term.as_ref() {
            let starts = self.term_starts(start_term);
            if starts.len() < self.term_starts(prefix).len() {
                let length = prefix.words.len();
                for &start in starts.iter() {
                    let Some(context) = start
                        .checked_sub(length)
                        .and_then(|at| self.spelled_term_at(prefix, at, allow_across_lines))
                    else {
                        continue;
                    };
                    if let Some(answer) = consider(context) {
                        return answer;
                    }
                }
                return None;
            }
        }
        let mut start_from = 0;
        while start_from < self.index.words.len() {
            let context = self.first_spelled_term_from(
                prefix_term.as_deref().unwrap_or(start_term),
                start_from,
                allow_across_lines,
            )?;
            start_from = context.first_word + 1;
            if let Some(answer) = consider(context) {
                return answer;
            }
        }
        None
    }
}

fn fragment_spelling_with_mode(value: &str, preserve_punctuation_spacing: bool) -> String {
    static MARKDOWN: OnceLock<Regex> = OnceLock::new();
    static OPENING_QUOTE_SPACE: OnceLock<Regex> = OnceLock::new();
    static OPENING_BRACKET_SPACE: OnceLock<Regex> = OnceLock::new();
    static CLOSING_SPACE: OnceLock<Regex> = OnceLock::new();
    let normalized = normalize_blank_whitespace(value);
    // Text with no emphasis marks, no opening quotation mark or bracket and no space before a closing one is
    // what the replacements below leave it.
    let mut space_before_closing = false;
    let mut after_space = false;
    for character in normalized.chars() {
        if after_space && matches!(character, '’' | '”' | ']' | ')' | ',' | '.' | ';' | ':' | '!' | '?') {
            space_before_closing = true;
            break;
        }
        after_space = character == ' ';
    }
    if !normalized.contains(['*', '_', '`'])
        && (preserve_punctuation_spacing
            || (!normalized.contains(['‘', '“', '[', '(']) && !space_before_closing))
    {
        return normalized;
    }
    let unmarked = js_regex(r"[*_`]", &MARKDOWN)
        .replace_all(&normalized, "")
        .into_owned();
    if preserve_punctuation_spacing {
        return unmarked;
    }
    let joined = js_regex(r"([‘“]) +", &OPENING_QUOTE_SPACE)
        .replace_all(&unmarked, "$1")
        .into_owned();
    let joined = js_regex(r"([\[(]) +", &OPENING_BRACKET_SPACE)
        .replace_all(&joined, "$1")
        .into_owned();
    js_regex(r" +([’”\]\),.;:!?])", &CLOSING_SPACE)
        .replace_all(&joined, "$1")
        .into_owned()
}

fn locate_document_quote(
    block: FragmentText<'_>,
    document: FragmentText<'_>,
    selected: PhraseSpan,
) -> Option<PhraseSpan> {
    let block_words = block.tokens();
    let document_words = document.tokens();
    let wanted = block_words
        .iter()
        .map(|word| block.slice(word.start, word.end).to_lowercase())
        .collect::<Vec<_>>();
    let mut previous = None;
    for window in [0, 2, 4, 8, 16, 32, 64, block_words.len()] {
        let first = selected.first_word.saturating_sub(window);
        let last = (selected.last_word + window).min(block_words.len().saturating_sub(1));
        if previous.replace((first, last)) == Some((first, last)) {
            continue;
        }
        let matches = document.phrase_spans(
            &wanted[first..=last],
            PhraseOptions {
                limit: Some(2),
                ..Default::default()
            },
        );
        if matches.len() != 1 {
            continue;
        }
        let first_word = matches[0].first_word + selected.first_word - first;
        let last_word = first_word + selected.last_word - selected.first_word;
        let leading = block.slice(selected.start, block_words[selected.first_word].start);
        let trailing = block.slice(block_words[selected.last_word].end, selected.end);
        let mut start = document_words.get(first_word)?.start;
        let mut end = document_words.get(last_word)?.end;
        let leading_length = utf16_len(leading);
        if !leading.is_empty()
            && start
                .checked_sub(leading_length)
                .is_some_and(|from| document.slice(from, start) == leading)
        {
            start -= leading_length;
        }
        if !trailing.is_empty() && document.slice(end, end + utf16_len(trailing)) == trailing {
            end += utf16_len(trailing);
        }
        return Some(PhraseSpan {
            start,
            end,
            first_word,
            last_word,
        });
    }
    None
}

fn opens_markdown(gap: &str) -> bool {
    static MARKER: OnceLock<Regex> = OnceLock::new();
    let Some(marker) = js_regex(r"(?u)(?:\*{1,3}|_{1,3}|`+)\s*$", &MARKER).find(gap) else {
        return false;
    };
    marker.start() > 0
        || !marker
            .as_str()
            .chars()
            .last()
            .is_some_and(javascript_whitespace)
}

fn ends_legal_reference(replay: &BrowserReplay<'_>, word: usize) -> bool {
    static REFERENCE: OnceLock<Regex> = OnceLock::new();
    static LOCATOR_CONTINUATION: OnceLock<Regex> = OnceLock::new();
    static LOCATOR_PART: OnceLock<Regex> = OnceLock::new();
    let source = replay.document.tokens();
    if word == 0 || word >= source.len() {
        return false;
    }
    if let Some(next) = source.get(word + 1) {
        let gap = replay.document.slice(source[word].end, next.start);
        if gap.chars().any(|character| matches!(character, '.' | '('))
            && js_regex(r"(?u)^\s*(?:\.|\)\s*\()?\s*\(?\s*$", &LOCATOR_CONTINUATION).is_match(gap)
            && js_regex(r"(?iu)^(?:\d+|[a-z]{1,5})$", &LOCATOR_PART)
                .is_match(replay.document.slice(next.start, next.end))
        {
            return false;
        }
    }
    let end = source[word].end;
    let through = source
        .get(word + 1)
        .map_or(replay.document.utf16_len(), |next| next.start);
    // A statute's name ("Criminal Code", "the Charter"), which the publisher's citator links.
    if replay.document.legal_reference_ends_within(source[word].start, through) {
        return true;
    }
    let mut from = end.saturating_sub(96);
    while from < end && replay.document.try_slice(from, through).is_none() {
        from += 1;
    }
    js_regex(
        r"(?iu)(?:\b(?:sections?|subsections?|paragraphs?|subparagraphs?|rules?)|\bs{1,2}\.)\s+\d+(?:\.\d+)*(?:\s*\(\s*[\p{L}\p{N}.-]{1,12}\s*\))*[.,;:]?\s*$",
        &REFERENCE,
    )
    .is_match(replay.document.slice(from, through))
}

fn oxford_role_series_seams(replay: &BrowserReplay<'_>, desired: PhraseSpan) -> HashSet<usize> {
    let source = replay.document.tokens();
    let mut seams = HashSet::new();
    let comma_after = |word: usize| {
        word < desired.last_word
            && replay
                .document
                .slice(source[word].end, source[word + 1].start)
                .contains(',')
    };
    let mut cue = desired.first_word;
    while cue + 6 <= desired.last_word {
        if !replay.literal_word_is(cue, "as") || !replay.literal_word_is(cue + 1, "the") {
            cue += 1;
            continue;
        }
        let mut first_comma = cue + 2;
        while first_comma < desired.last_word && !comma_after(first_comma) {
            first_comma += 1;
        }
        if first_comma >= desired.last_word || !replay.literal_word_is(first_comma + 1, "the") {
            cue += 1;
            continue;
        }
        let mut second_comma = first_comma + 2;
        while second_comma < desired.last_word && !comma_after(second_comma) {
            second_comma += 1;
        }
        let conjunction = second_comma + 1;
        let article = conjunction + 1;
        if second_comma >= desired.last_word
            || !replay.literal_word_is(conjunction, "or")
            || !(replay.literal_word_is(article, "a")
                || replay.literal_word_is(article, "an")
                || replay.literal_word_is(article, "the"))
            || article >= desired.last_word
            || replay
                .document
                .slice(source[cue].start, source[article].end)
                .chars()
                .any(|character| matches!(character, '\r' | '\n'))
        {
            cue += 1;
            continue;
        }
        seams.extend([cue, first_comma, conjunction]);
        cue = article + 1;
    }
    seams
}

fn line_start_furniture_last_word(
    document: FragmentText<'_>,
    piece_first_word: usize,
    piece_last_word: usize,
    markdown_labels: bool,
) -> Option<usize> {
    let words = document.tokens();
    let before = document.slice(0, words[piece_first_word].start);
    let line_start_byte = before
        .rfind(|character| matches!(character, '\n' | '\r'))
        .map_or(0, |byte| {
            byte + before[byte..].chars().next().unwrap().len_utf8()
        });
    let line_start = utf16_len(&before[..line_start_byte]);
    let line = document.slice(line_start, words[piece_last_word].end);
    let indent = line
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .count();
    let marker_length = leading_label_length_with_mode(&line[indent..], markdown_labels)
        .max(document.paragraph_marker_length(line_start + indent));
    if marker_length == 0 {
        return None;
    }
    let first_target = word_at_or_after(
        document,
        line_start + indent + marker_length,
        piece_first_word,
    )?;
    (first_target > piece_first_word && first_target <= piece_last_word).then_some(first_target - 1)
}

#[derive(Clone, Copy)]
struct MaximalCorePiece {
    start: usize,
    end: usize,
    first_word: usize,
    last_word: usize,
    context_first_word: usize,
    context_last_word: usize,
}

fn source_line_break_between(replay: &BrowserReplay<'_>, left: usize, right: usize) -> bool {
    let words = replay.document.tokens();
    replay
        .document
        .slice(words[left].end, words[right].start)
        .chars()
        .any(|character| matches!(character, '\r' | '\n'))
}

fn source_line_first_word(replay: &BrowserReplay<'_>, word: usize) -> usize {
    let mut first = word;
    while first > 0 && !source_line_break_between(replay, first - 1, first) {
        first -= 1;
    }
    first
}

fn source_line_last_word(replay: &BrowserReplay<'_>, word: usize) -> usize {
    let words = replay.document.tokens();
    let mut last = word;
    while last + 1 < words.len() && !source_line_break_between(replay, last, last + 1) {
        last += 1;
    }
    last
}

#[derive(Clone)]
struct BuiltPiece {
    directive: String,
    quote_index: usize,
    start: usize,
    end: usize,
    first_word: usize,
    last_word: usize,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFragmentWordInterval {
    pub quote_index: usize,
    pub start: usize,
    pub end: usize,
    pub first_word: usize,
    pub last_word: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFragmentPlan {
    pub directives: Vec<String>,
    pub source_word_intervals: Vec<TextFragmentWordInterval>,
    pub paint_quotes: Vec<String>,
    /// Every required source word is assigned to a directive. Browser paint
    /// remains a separate integration proof over the emitted URL.
    pub source_safe_complete: bool,
    pub painted_words: usize,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum LexicalForm {
    Word,
    Atom,
    Run,
}

#[derive(Clone)]
struct LexicalTerm {
    text: String,
    start: usize,
    end: usize,
    first_word: usize,
    last_word: usize,
    form: LexicalForm,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct PieceKey {
    quote_index: usize,
    start: usize,
    end: usize,
    first_word: usize,
    last_word: usize,
    context_first_word: usize,
    context_last_word: usize,
}

#[derive(Clone)]
struct Candidate {
    piece: BuiltPiece,
    encoded_length: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FurnitureKind {
    Label,
    Metadata,
}

struct MaximalPlanner<'replay, 'document> {
    replay: &'replay BrowserReplay<'document>,
    pdf: bool,
    publisher_may_annotate_legal_reference: bool,
    split_html_source_blocks: bool,
    /// The page renders the document's own text: no seam prints otherwise there.
    source_page: bool,
    /// The page may print a passage again where the document holds it once (CanLII's reporter headnotes, its
    /// "[See Note]" lines): a quotation inside a paragraph is told apart by the words before it, a passage opening its
    /// paragraph by its printed label.
    repeating_page: bool,
    atom_cache: HashMap<usize, LexicalTerm>,
    atomic_options_cache: HashMap<usize, Vec<LexicalTerm>>,
    /// endpoint_terms_from_start and _end by (from the start, first word, last word, context): a range plan reads a
    /// piece's endpoints once for each way it pairs them.
    endpoint_cache: HashMap<(bool, usize, usize, bool), Vec<LexicalTerm>>,
    directive_cache: HashMap<PieceKey, Option<String>>,
    furniture_cache: HashMap<usize, Option<FurnitureKind>>,
}

impl PieceKey {
    fn new(quote_index: usize, piece: MaximalCorePiece) -> Self {
        Self {
            quote_index,
            start: piece.start,
            end: piece.end,
            first_word: piece.first_word,
            last_word: piece.last_word,
            context_first_word: piece.context_first_word,
            context_last_word: piece.context_last_word,
        }
    }
}

fn selects(piece: MaximalCorePiece, selected: Option<PhraseSpan>) -> bool {
    selected.is_some_and(|selected| {
        selected.first_word == piece.first_word && selected.last_word == piece.last_word
    })
}

impl<'replay, 'document> MaximalPlanner<'replay, 'document> {
    fn new(
        replay: &'replay BrowserReplay<'document>,
        pdf: bool,
        publisher_may_annotate_legal_reference: bool,
        split_html_source_blocks: bool,
        source_page: bool,
        repeating_page: bool,
    ) -> Self {
        Self {
            replay,
            pdf,
            publisher_may_annotate_legal_reference,
            split_html_source_blocks,
            source_page,
            repeating_page,
            atom_cache: HashMap::new(),
            atomic_options_cache: HashMap::new(),
            endpoint_cache: HashMap::new(),
            directive_cache: HashMap::new(),
            furniture_cache: HashMap::new(),
        }
    }

    fn line_first_word(&self, word: usize) -> usize {
        source_line_first_word(self.replay, word)
    }

    /// Whether `piece` opens its paragraph after the printed label its context starts at, so a plan of it is told
    /// apart by that label: a page may print the passage again where the provider's copy does not (a reporter's
    /// headnote on CanLII), and only the label tells the paragraph's copy apart. Measured on every cached page
    /// (2026-10-18, 3,690 links): painted elsewhere 10 -> 3 and on target +8, none worse.
    fn labelled_opening(&mut self, piece: MaximalCorePiece) -> bool {
        // Only where the page may repeat the passage: elsewhere the page is the document (bccourts' 1999 template and
        // e-Laws print the number otherwise, and lost 27 of 45 labelled links, 2026-10-18).
        self.repeating_page && !self.pdf && !self.source_page && piece.first_word > piece.context_first_word
            && (piece.context_first_word..piece.first_word).all(|word| self.source_furniture(word) == Some(FurnitureKind::Label))
    }

    /// Whether the page prints the label before `word` against it ("57An application"): the document sets the two
    /// apart by whitespace alone, on a page that glues its section labels.
    fn glued_label_before(&mut self, word: usize) -> bool {
        if word == 0 || !self.replay.document.glues_labels()
            || self.source_furniture(word - 1) != Some(FurnitureKind::Label)
        {
            return false;
        }
        let words = self.replay.document.tokens();
        self.replay.document.slice(words[word - 1].end, words[word].start).chars().all(javascript_whitespace)
    }

    /// Whether a term holds a seam the page may print otherwise ([`has_seam`]): never on a page of the source text.
    fn seamed(&self, text: &str) -> bool {
        !self.source_page && has_seam(text)
    }

    fn line_last_word(&self, word: usize) -> usize {
        source_line_last_word(self.replay, word)
    }

    fn atom_at(&mut self, word: usize) -> Option<LexicalTerm> {
        if let Some(cached) = self.atom_cache.get(&word) {
            return Some(cached.clone());
        }
        let words = self.replay.document.tokens();
        words.get(word)?;
        let mut first_word = word;
        let mut last_word = word;
        while first_word > 0
            && !self
                .replay
                .document
                .slice(words[first_word - 1].end, words[first_word].start)
                .chars()
                .any(javascript_whitespace)
        {
            first_word -= 1;
        }
        while last_word + 1 < words.len()
            && !self
                .replay
                .document
                .slice(words[last_word].end, words[last_word + 1].start)
                .chars()
                .any(javascript_whitespace)
        {
            last_word += 1;
        }

        let left_boundary = first_word
            .checked_sub(1)
            .map_or(0, |previous| words[previous].end);
        let left_gap = self
            .replay
            .document
            .slice(left_boundary, words[first_word].start);
        let start = left_gap
            .char_indices()
            .filter(|(_, character)| javascript_whitespace(*character))
            .last()
            .map_or(left_boundary, |(byte, character)| {
                left_boundary + utf16_len(&left_gap[..byte + character.len_utf8()])
            });
        let right_boundary = words
            .get(last_word + 1)
            .map_or(self.replay.document.utf16_len(), |next| next.start);
        let right_gap = self
            .replay
            .document
            .slice(words[last_word].end, right_boundary);
        let end = right_gap
            .char_indices()
            .find(|(_, character)| javascript_whitespace(*character))
            .map_or(right_boundary, |(byte, _)| {
                words[last_word].end + utf16_len(&right_gap[..byte])
            });
        let term = LexicalTerm {
            text: self
                .replay
                .fragment_spelling(self.replay.document.slice(start, end)),
            start,
            end,
            first_word,
            last_word,
            form: LexicalForm::Atom,
        };
        for index in first_word..=last_word {
            self.atom_cache.insert(index, term.clone());
        }
        Some(term)
    }

    fn atomic_options_at(&mut self, word: usize) -> Vec<LexicalTerm> {
        if let Some(cached) = self.atomic_options_cache.get(&word) {
            return cached.clone();
        }
        let words = self.replay.document.tokens();
        let Some(source_word) = words.get(word) else {
            return Vec::new();
        };
        let bare = LexicalTerm {
            text: self.replay.fragment_spelling(
                self.replay
                    .document
                    .slice(source_word.start, source_word.end),
            ),
            start: source_word.start,
            end: source_word.end,
            first_word: word,
            last_word: word,
            form: LexicalForm::Word,
        };
        let atom = self.atom_at(word).unwrap_or_else(|| bare.clone());
        let options = if atom.text != bare.text
            || atom.start != bare.start
            || atom.end != bare.end
            || atom.first_word != bare.first_word
            || atom.last_word != bare.last_word
        {
            vec![bare, atom]
        } else {
            vec![bare]
        };
        self.atomic_options_cache.insert(word, options.clone());
        options
    }

    fn safe_lexical_gap(
        &self,
        left: &LexicalTerm,
        right: &LexicalTerm,
        allow_line_break: bool,
    ) -> bool {
        let gap = self.replay.document.slice(left.end, right.start);
        // On a page each of the text's lines is a block of its own, which no term crosses; a PDF's lines are not.
        (allow_line_break && self.pdf
            || !gap
                .chars()
                .any(|character| matches!(character, '\r' | '\n')))
            && !opens_markdown(gap)
    }

    fn has_structural_seam(&self, left_word: usize, right_word: usize) -> bool {
        static BILINGUAL_RECORD: OnceLock<Regex> = OnceLock::new();
        static PARENTHETICAL_MARKDOWN: OnceLock<Regex> = OnceLock::new();
        let words = self.replay.document.tokens();
        let gap = self
            .replay
            .document
            .slice(words[left_word].end, words[right_word].start);
        if gap.contains(['\r', '\n'])
            && self
                .replay
                .document
                .paragraph_marker_length(words[right_word].start)
                > 0
        {
            return true;
        }
        if self.split_html_source_blocks
            && gap
                .chars()
                .any(|character| matches!(character, '\r' | '\n'))
        {
            return true;
        }
        if self.pdf
            && (gap
                .chars()
                .any(|character| matches!(character, '\r' | '\n'))
                || js_regex(r#"(?u)(?:»|Â»)\s*[“\"]"#, &BILINGUAL_RECORD).is_match(gap))
        {
            return true;
        }
        opens_markdown(gap)
            && !js_regex(
                r"(?u)\(\s*(?:\*{1,3}|_{1,3}|`+)\s*$",
                &PARENTHETICAL_MARKDOWN,
            )
            .is_match(gap)
    }

    /// Whether a context term meeting the piece at `left_word` | `right_word` meets it at a seam a page may print
    /// otherwise (a sentence's end, "at para."): a prefix ending at one finds no start after it on such a page.
    fn boundary_seam(&self, left_word: usize, right_word: usize) -> bool {
        let words = self.replay.document.tokens();
        let left = self.replay.document.slice(words[left_word].start, words[left_word].end);
        let right = self.replay.document.slice(words[right_word].start, words[right_word].end);
        !self.source_page && (sentence_end(left, right) || spaced_apart(left, right))
    }

    fn exact_target_for(&mut self, piece: MaximalCorePiece) -> Option<LexicalTerm> {
        for word in piece.first_word..piece.last_word {
            if self.has_structural_seam(word, word + 1)
                || !self.pdf && source_line_break_between(self.replay, word, word + 1)
            {
                return None;
            }
        }
        let words = self.replay.document.tokens();
        let first_atom = self.atom_at(piece.first_word)?;
        let last_atom = self.atom_at(piece.last_word)?;
        let start = if first_atom.first_word == piece.first_word
            && first_atom.last_word > piece.first_word
            && first_atom.last_word <= piece.last_word
        {
            first_atom.start
        } else {
            words[piece.first_word].start
        };
        let end = if last_atom.last_word == piece.last_word
            && last_atom.first_word < piece.last_word
            && last_atom.first_word >= piece.first_word
        {
            last_atom.end
        } else {
            words[piece.last_word].end
        };
        Some(LexicalTerm {
            text: self
                .replay
                .fragment_spelling(self.replay.document.slice(start, end)),
            start,
            end,
            first_word: piece.first_word,
            last_word: piece.last_word,
            form: if piece.first_word == piece.last_word {
                LexicalForm::Word
            } else {
                LexicalForm::Run
            },
        })
    }

    fn replay_exact_text(&self, target: &str, prefix: &str, suffix: &str) -> ExactDirectiveReplay {
        self.replay
            .spelled_term(target)
            .map_or_else(ExactDirectiveReplay::default, |term| {
                self.replay.replay_exact(
                    &term,
                    prefix,
                    suffix,
                    !self.pdf || !prefix.is_empty() || !suffix.is_empty(),
                )
            })
    }

    fn replay_range_text(
        &self,
        start: &str,
        end: &str,
        prefix: &str,
        suffix: &str,
    ) -> Option<PhraseSpan> {
        self.replay.replay_range(
            &*self.replay.spelled_term(start)?,
            &*self.replay.spelled_term(end)?,
            prefix,
            suffix,
            false,
        )
    }

    fn endpoint_terms_from_start(&mut self, piece: MaximalCorePiece, context: bool) -> Vec<LexicalTerm> {
        let key = (true, piece.first_word, piece.last_word, context);
        if let Some(terms) = self.endpoint_cache.get(&key) {
            return terms.clone();
        }
        let terms = self.read_endpoint_terms_from_start(piece, context);
        self.endpoint_cache.insert(key, terms.clone());
        terms
    }

    fn endpoint_terms_from_end(&mut self, piece: MaximalCorePiece, context: bool) -> Vec<LexicalTerm> {
        let key = (false, piece.first_word, piece.last_word, context);
        if let Some(terms) = self.endpoint_cache.get(&key) {
            return terms.clone();
        }
        let terms = self.read_endpoint_terms_from_end(piece, context);
        self.endpoint_cache.insert(key, terms.clone());
        terms
    }

    fn read_endpoint_terms_from_start(
        &mut self,
        piece: MaximalCorePiece,
        context: bool,
    ) -> Vec<LexicalTerm> {
        let Some(first_atom) = self.atom_at(piece.first_word) else {
            return Vec::new();
        };
        if first_atom.first_word != piece.first_word || first_atom.last_word > piece.last_word {
            return Vec::new();
        }
        let options = self.atomic_options_at(piece.first_word);
        let mut terms = if options.len() > 1 {
            vec![first_atom.clone()]
        } else {
            options
        };
        let mut last = first_atom.clone();
        while last.last_word < piece.last_word {
            let Some(next) = self.atom_at(last.last_word + 1) else {
                break;
            };
            if next.last_word > piece.last_word || !self.safe_lexical_gap(&last, &next, context) {
                break;
            }
            last = next;
            terms.push(LexicalTerm {
                text: self
                    .replay
                    .fragment_spelling(self.replay.document.slice(first_atom.start, last.end)),
                start: first_atom.start,
                end: last.end,
                first_word: piece.first_word,
                last_word: last.last_word,
                form: LexicalForm::Run,
            });
        }
        terms
    }

    fn read_endpoint_terms_from_end(
        &mut self,
        piece: MaximalCorePiece,
        context: bool,
    ) -> Vec<LexicalTerm> {
        let Some(last_atom) = self.atom_at(piece.last_word) else {
            return Vec::new();
        };
        if last_atom.last_word != piece.last_word || last_atom.first_word < piece.first_word {
            return Vec::new();
        }
        let options = self.atomic_options_at(piece.last_word);
        let mut terms = if options.len() > 1 {
            vec![last_atom.clone()]
        } else {
            options
        };
        let mut first = last_atom.clone();
        while first.first_word > piece.first_word {
            let Some(previous) = self.atom_at(first.first_word - 1) else {
                break;
            };
            if previous.first_word < piece.first_word
                || !self.safe_lexical_gap(&previous, &first, context)
            {
                break;
            }
            first = previous;
            terms.push(LexicalTerm {
                text: self
                    .replay
                    .fragment_spelling(self.replay.document.slice(first.start, last_atom.end)),
                start: first.start,
                end: last_atom.end,
                first_word: first.first_word,
                last_word: piece.last_word,
                form: LexicalForm::Run,
            });
        }
        terms
    }

    fn trim_leading_furniture(&mut self, span: PhraseSpan) -> Option<PhraseSpan> {
        static BILINGUAL: OnceLock<Regex> = OnceLock::new();
        let source = self.replay.document.slice(span.start, span.end);
        let marker_length = leading_label_length_with_mode(source, self.split_html_source_blocks)
            .max(self.replay.document.paragraph_marker_length(span.start))
            .max(if self.split_html_source_blocks {
                let markdown = markdown_label_length(source);
                js_regex(r"(?u)^\(?\s*[^)]*(?:»|Â»)[^)]*\)\s*", &BILINGUAL)
                    .find(source)
                    .map_or(markdown, |found| {
                        markdown.max(utf16_len(&source[..found.end()]))
                    })
            } else {
                0
            });
        if marker_length == 0 {
            return Some(span);
        }
        let first_word = word_at_or_after(
            self.replay.document,
            span.start + marker_length,
            span.first_word,
        )?;
        if first_word < span.first_word || first_word > span.last_word {
            return None;
        }
        // A word run into a tight token before it stays with it, unless that token is the label itself: a provider
        // may glue the label to the first word ("[82]The cases"), which a page prints apart.
        let atom = self.atom_at(first_word)?;
        if atom.first_word < first_word && atom.start < span.start {
            return Some(span);
        }
        // A page that prints the label against the word after it leaves no word boundary before that word: the
        // passage starts at the next.
        let first_word = if first_word < span.last_word && self.glued_label_before(first_word) {
            first_word + 1
        } else {
            first_word
        };
        let words = self.replay.document.tokens();
        Some(PhraseSpan {
            start: words[first_word].start,
            first_word,
            ..span
        })
    }

    fn core_piece(
        &self,
        span: PhraseSpan,
        clamp_leading_context: bool,
        clamp_trailing_context: bool,
    ) -> MaximalCorePiece {
        let words = self.replay.document.tokens();
        let same_line_first = self.line_first_word(span.first_word);
        let same_line_last = self.line_last_word(span.last_word);
        let adjacent_first = if self.split_html_source_blocks {
            let mut first = span.first_word;
            while first > 0 && !self.has_structural_seam(first - 1, first) {
                first -= 1;
            }
            first
        } else if same_line_first > 0 {
            self.line_first_word(same_line_first - 1)
        } else {
            same_line_first
        };
        let adjacent_last = if self.split_html_source_blocks {
            let mut last = span.last_word;
            while last + 1 < words.len() && !self.has_structural_seam(last, last + 1) {
                last += 1;
            }
            last
        } else if same_line_last + 1 < words.len() {
            self.line_last_word(same_line_last + 1)
        } else {
            same_line_last
        };
        MaximalCorePiece {
            start: span.start,
            end: span.end,
            first_word: span.first_word,
            last_word: span.last_word,
            // A piece whose printed label was trimmed takes context only from its own line: the label alone, which
            // context_allowed admits as a term of its own (a passage a headnote repeats is told apart by its number).
            context_first_word: if clamp_leading_context {
                same_line_first.min(span.first_word)
            } else {
                adjacent_first
            },
            context_last_word: if clamp_trailing_context {
                span.last_word
            } else {
                adjacent_last
            },
        }
    }

    fn source_pieces(
        &mut self,
        desired: PhraseSpan,
        clamp_leading_context: bool,
        clamp_trailing_context: bool,
        exact_members: bool,
    ) -> Vec<MaximalCorePiece> {
        static BILINGUAL_OPEN: OnceLock<Regex> = OnceLock::new();
        static BILINGUAL_CLOSE: OnceLock<Regex> = OnceLock::new();
        let words = self.replay.document.tokens();
        let source_only_seams = oxford_role_series_seams(self.replay, desired);
        let mut pieces = Vec::new();
        let mut first_word = desired.first_word;
        let add = |planner: &mut Self,
                   first_word: usize,
                   last_word: usize,
                   pieces: &mut Vec<MaximalCorePiece>| {
            if first_word > last_word {
                return;
            }
            let start = if first_word == desired.first_word {
                desired.start
            } else {
                let gap = planner
                    .replay
                    .document
                    .slice(words[first_word - 1].end, words[first_word].start);
                let after_line_marker = gap
                    .char_indices()
                    .filter(|(_, character)| matches!(character, '\r' | '\n'))
                    .last();
                if let Some((byte, character)) = after_line_marker {
                    words[first_word - 1].end + utf16_len(&gap[..byte + character.len_utf8()])
                } else {
                    words[first_word].start
                }
            };
            let raw = PhraseSpan {
                start,
                end: if last_word == desired.last_word {
                    desired.end
                } else {
                    words[last_word].end
                },
                first_word,
                last_word,
            };
            if !exact_members && planner.split_html_source_blocks && first_word > 0 && last_word + 1 < words.len() {
                let left = planner
                    .replay
                    .document
                    .slice(words[first_word - 1].end, raw.start);
                let right = planner
                    .replay
                    .document
                    .slice(raw.end, words[last_word + 1].start);
                if js_regex(r"(?u)\(\s*(?:«|Â«)\s*$", &BILINGUAL_OPEN).is_match(left)
                    && js_regex(r"(?u)^\s*(?:»|Â»)\s*\)", &BILINGUAL_CLOSE).is_match(right)
                {
                    return;
                }
            }
            if let Some(trimmed) = if exact_members { Some(raw) } else { planner.trim_leading_furniture(raw) } {
                let trimmed_furniture = trimmed.first_word != raw.first_word;
                let markdown_furniture = trimmed_furniture
                    && planner
                        .replay
                        .document
                        .slice(raw.start, trimmed.start)
                        .trim_start()
                        .starts_with(['*', '_']);
                let mut piece = planner.core_piece(
                    trimmed,
                    clamp_leading_context && first_word == desired.first_word
                        || trimmed_furniture && !planner.split_html_source_blocks,
                    clamp_trailing_context && last_word == desired.last_word,
                );
                if planner.split_html_source_blocks && trimmed_furniture {
                    piece.context_first_word =
                        if markdown_furniture || first_word != desired.first_word {
                            piece.first_word
                        } else {
                            raw.first_word
                        };
                } else if planner.split_html_source_blocks
                    && clamp_leading_context
                    && first_word == desired.first_word
                    && piece.first_word > 0
                    && planner.source_furniture(piece.first_word - 1) == Some(FurnitureKind::Label)
                {
                    let label = planner.line_first_word(piece.first_word);
                    let label_prefix = planner.replay.document.slice(
                        planner.replay.document.tokens()[label].start,
                        planner.replay.document.tokens()[piece.first_word].start,
                    );
                    if !label_prefix.trim_start().starts_with(['*', '_']) {
                        piece.context_first_word = label;
                    }
                }
                if trimmed_furniture && piece.first_word > 0 && planner.glued_label_before(piece.first_word - 1) {
                    // The word glued to the label is no context either: no prefix before the passage.
                    piece.context_first_word = piece.first_word;
                }
                pieces.push(piece);
            }
        };
        for word in desired.first_word..desired.last_word {
            if !self.has_structural_seam(word, word + 1)
                && !(self.publisher_may_annotate_legal_reference
                    && ends_legal_reference(self.replay, word))
                && !source_only_seams.contains(&word)
            {
                continue;
            }
            add(self, first_word, word, &mut pieces);
            first_word = word + 1;
        }
        add(self, first_word, desired.last_word, &mut pieces);
        pieces
    }

    fn source_furniture(&mut self, word: usize) -> Option<FurnitureKind> {
        static METADATA: OnceLock<Regex> = OnceLock::new();
        if let Some(cached) = self.furniture_cache.get(&word) {
            return *cached;
        }
        let words = self.replay.document.tokens();
        let first = self.line_first_word(word);
        let last = self.line_last_word(word);
        let marker_last = line_start_furniture_last_word(
            self.replay.document,
            first,
            last,
            self.split_html_source_blocks,
        );
        let line = self
            .replay
            .document
            .slice(words[first].start, words[last].end)
            .trim();
        let metadata = last - first < 12
            && js_regex(
                r"(?iu)^(?:citation|court files?|dockets?|registry|date|coram|heard|hearing|style of cause)\b\s*:?",
                &METADATA,
            )
            .is_match(line);
        for index in first..=last {
            self.furniture_cache.insert(
                index,
                if metadata {
                    Some(FurnitureKind::Metadata)
                } else if marker_last.is_some_and(|marker| index <= marker) {
                    Some(FurnitureKind::Label)
                } else {
                    None
                },
            );
        }
        self.furniture_cache.get(&word).copied().flatten()
    }

    fn context_allowed(
        &mut self,
        term: &LexicalTerm,
        desired: PhraseSpan,
        required: &HashSet<usize>,
    ) -> bool {
        // A printed label is context only alone: a page sets it apart from the words (an anchor, a run of no-break
        // spaces), so a browser finds it as a term of its own but not run into them.
        let label_only = (term.first_word..=term.last_word)
            .all(|word| self.source_furniture(word) == Some(FurnitureKind::Label));
        for word in term.first_word..=term.last_word {
            let furniture = self.source_furniture(word);
            if word >= desired.first_word && word <= desired.last_word {
                if !required.contains(&word)
                    && (furniture != Some(FurnitureKind::Label) || self.pdf)
                {
                    return false;
                }
            } else if furniture == Some(FurnitureKind::Metadata)
                || furniture == Some(FurnitureKind::Label) && !label_only && !self.source_page
            {
                return false;
            }
        }
        true
    }

    fn shortest_sufficient_context(
        &mut self,
        terms: Vec<LexicalTerm>,
        desired: PhraseSpan,
        required: &HashSet<usize>,
    ) -> Vec<LexicalTerm> {
        let mut allowed = Vec::new();
        for term in terms {
            // Terms come from the target outward, each a word longer, so once one holds a seam all after it do. A label
            // read whole across its spacing ("45 (1)") is no exception: offered as well as its last part, it let shorter
            // endpoints take the place of longer ones and failed where CanLII closes the label up ("9(1)"), 61 links
            // against 4 (measured 2026-10-18).
            if self.seamed(&term.text) {
                break;
            }
            if !self.context_allowed(&term, desired, required) {
                continue;
            }
            let unique = self
                .replay
                .spelled_term(&term.text)
                .is_some_and(|target| self.replay.replay_exact(&target, "", "", true).count == 1);
            allowed.push(term);
            if unique {
                break;
            }
        }
        allowed
    }

    fn context_class(
        &mut self,
        piece: MaximalCorePiece,
        before: Option<&LexicalTerm>,
        after: Option<&LexicalTerm>,
        requested_first: usize,
        requested_last: usize,
    ) -> usize {
        if before.is_none() && after.is_none() {
            return 0;
        }
        if before.is_some_and(|term| self.has_structural_seam(term.last_word, piece.first_word))
            || after.is_some_and(|term| self.has_structural_seam(piece.last_word, term.first_word))
        {
            return 3;
        }
        if before.is_some_and(|term| {
            term.first_word < requested_first || term.last_word > requested_last
        }) || after.is_some_and(|term| {
            term.first_word < requested_first || term.last_word > requested_last
        }) {
            2
        } else {
            1
        }
    }

    fn shortest_for(
        &mut self,
        piece: MaximalCorePiece,
        exact_targets: &[LexicalTerm],
        contexts: &[(Option<LexicalTerm>, Option<LexicalTerm>)],
    ) -> Option<String> {
        let mut shortest = None::<String>;
        let consider = |shortest: &mut Option<String>, directive: String| {
            if shortest
                .as_ref()
                .is_none_or(|current| directive.len() < current.len())
            {
                *shortest = Some(directive);
            }
        };
        let exact_targets = exact_targets.iter().filter(|target| !self.seamed(&target.text)).cloned().collect::<Vec<_>>();
        for target in &exact_targets {
            for (before, after) in contexts {
                if target.form == LexicalForm::Word
                    && before.is_none()
                    && after.is_none()
                    && !(self.pdf && self.replay_exact_text(&target.text, "", "").count == 1)
                {
                    continue;
                }
                let (before_text, after_text) = (before.as_ref().map_or("", |term| term.text.as_str()),
                    after.as_ref().map_or("", |term| term.text.as_str()));
                // The term without its outer marks where that selects, else with them (a mark may be what tells
                // the term apart: "Tribunal”)" once, "Tribunal" throughout).
                let shed = shed_outer_punctuation(&target.text, before.is_none(), after.is_none());
                let variants = if shed == target.text { vec![shed] } else { vec![shed, target.text.clone()] };
                for target_text in variants {
                    if !selects(piece, self.replay_exact_text(&target_text, before_text, after_text).first) {
                        continue;
                    }
                    consider(&mut shortest, text_directive(&target_text, before_text, after_text));
                    break;
                }
            }
        }
        shortest
    }

    fn atomic_directive_for(
        &mut self,
        piece: MaximalCorePiece,
        quote_index: usize,
        requested_first: usize,
        requested_last: usize,
        desired: PhraseSpan,
        required: &HashSet<usize>,
    ) -> Option<String> {
        let key = PieceKey::new(quote_index, piece);
        if let Some(cached) = self.directive_cache.get(&key) {
            return cached.clone();
        }
        let exact_targets = self.exact_target_for(piece).into_iter().collect::<Vec<_>>();
        let labelled = self.labelled_opening(piece);
        // On a page that may repeat the passage, a piece inside its paragraph is told apart by the words before it
        // where it has any (a reporter's headnote quotes the words, not what precedes them in the paragraph).
        let guarded = labelled || self.repeating_page && !self.pdf && !self.source_page
            && piece.first_word > piece.context_first_word;

        let bare = vec![(None, None)];
        let mut directive = if guarded { None } else { self.shortest_for(piece, &exact_targets, &bare) };
        if directive.is_none() {
            let words = self.replay.document.tokens();
            let prefix = if piece.first_word > piece.context_first_word {
                let context = MaximalCorePiece {
                    start: words[piece.context_first_word].start,
                    end: words[piece.first_word - 1].end,
                    first_word: piece.context_first_word,
                    last_word: piece.first_word - 1,
                    context_first_word: piece.context_first_word,
                    context_last_word: piece.first_word - 1,
                };
                let terms = self.endpoint_terms_from_end(context, true);
                self.shortest_sufficient_context(terms, desired, required)
            } else {
                Vec::new()
            };
            let suffix = if piece.last_word < piece.context_last_word {
                let context = MaximalCorePiece {
                    start: words[piece.last_word + 1].start,
                    end: words[piece.context_last_word].end,
                    first_word: piece.last_word + 1,
                    last_word: piece.context_last_word,
                    context_first_word: piece.last_word + 1,
                    context_last_word: piece.context_last_word,
                };
                let terms = self.endpoint_terms_from_start(context, true);
                self.shortest_sufficient_context(terms, desired, required)
            } else {
                Vec::new()
            };
            // A context meeting the piece at a seam is read only where no other tells the piece apart: a sentence a
            // headnote repeats whole is told apart by the sentence before it, or painted nowhere.
            let (seamed_prefix, mut prefix): (Vec<_>, Vec<_>) = prefix.into_iter()
                .partition(|term| self.boundary_seam(term.last_word, piece.first_word));
            let (seamed_suffix, mut suffix): (Vec<_>, Vec<_>) = suffix.into_iter()
                .partition(|term| self.boundary_seam(piece.last_word, term.first_word));
            for seamed in [false, true] {
                if seamed {
                    if seamed_prefix.is_empty() && seamed_suffix.is_empty() {
                        break;
                    }
                    prefix.extend(seamed_prefix.iter().cloned());
                    suffix.extend(seamed_suffix.iter().cloned());
                }
                // A guarded piece with no prefix to take (its words open their line) is planned as before.
                let guarded = guarded && (labelled || !prefix.is_empty());
                let mut prefix = prefix.clone();
                if guarded && !labelled {
                    // Two words or more where the prefix has them: one word may recur before a headnote's copy.
                    let longer = prefix.iter().filter(|term| term.last_word > term.first_word).cloned().collect::<Vec<_>>();
                    if !longer.is_empty() {
                        prefix = longer;
                    }
                }
                if self.split_html_source_blocks && piece.first_word != desired.first_word && !guarded {
                    let suffix_only = suffix
                        .iter()
                        .cloned()
                        .map(|after| (None, Some(after)))
                        .collect::<Vec<_>>();
                    directive = self.shortest_for(piece, &exact_targets, &suffix_only);
                }
                let mut contexts_by_class = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
                for before in &prefix {
                    let class =
                        self.context_class(piece, Some(before), None, requested_first, requested_last);
                    contexts_by_class[class].push((Some(before.clone()), None));
                }
                for after in &suffix {
                    let class =
                        self.context_class(piece, None, Some(after), requested_first, requested_last);
                    contexts_by_class[class].push((None, Some(after.clone())));
                }
                for before in &prefix {
                    for after in &suffix {
                        let class = self.context_class(
                            piece,
                            Some(before),
                            Some(after),
                            requested_first,
                            requested_last,
                        );
                        contexts_by_class[class].push((Some(before.clone()), Some(after.clone())));
                    }
                }
                if guarded {
                    for contexts in &mut contexts_by_class {
                        contexts.retain(|(before, _)| before.is_some());
                    }
                }
                for contexts in contexts_by_class.iter().skip(1) {
                    if directive.is_some() {
                        break;
                    }
                    directive = self.shortest_for(piece, &exact_targets, contexts);
                    if directive.is_some() {
                        break;
                    }
                }
                if directive.is_some() {
                    break;
                }
            }
        }
        self.directive_cache.insert(key, directive.clone());
        directive
    }

    /// The piece, then the piece read on to its sentence's end, back to its sentence's start, and both, within its
    /// line and short of a printed label: where only a context past a seam tells the piece apart (a passage a headnote
    /// repeats but for its next sentence, or its paragraph's number), the piece read to the seam takes that context
    /// as a term of its own, at the cost of painting the rest of the sentence.
    fn sentence_extensions(&mut self, piece: MaximalCorePiece) -> Vec<MaximalCorePiece> {
        let words = self.replay.document.tokens();
        let seam = |planner: &mut Self, left: usize| {
            has_seam(planner.replay.document.slice(words[left].start, words[left + 1].end))
                || source_line_break_between(planner.replay, left, left + 1)
                || planner.has_structural_seam(left, left + 1)
                || [left, left + 1].iter().any(|&word| planner.source_furniture(word) == Some(FurnitureKind::Label))
        };
        let mut last = piece.last_word;
        while last < piece.context_last_word && !seam(self, last) {
            last += 1;
        }
        let mut first = piece.first_word;
        while first > piece.context_first_word && !seam(self, first - 1) {
            first -= 1;
        }
        let read = |first_word: usize, last_word: usize| MaximalCorePiece {
            start: words[first_word].start, end: words[last_word].end, first_word, last_word, ..piece
        };
        let mut pieces = vec![piece];
        for extended in [read(piece.first_word, last), read(first, piece.last_word), read(first, last)] {
            if !pieces.iter().any(|other| other.first_word == extended.first_word && other.last_word == extended.last_word) {
                pieces.push(extended);
            }
        }
        pieces
    }

    /// The piece's parts between its seams ([`has_seam`]), where it has any.
    fn seam_parts(&self, piece: MaximalCorePiece) -> Vec<MaximalCorePiece> {
        let words = self.replay.document.tokens();
        let mut parts = Vec::new();
        let mut first = piece.first_word;
        for left in piece.first_word..piece.last_word {
            if has_seam(self.replay.document.slice(words[left].start, words[left + 1].end)) {
                parts.push(MaximalCorePiece { start: words[first].start, end: words[left].end, first_word: first,
                    last_word: left, ..piece });
                first = left + 1;
            }
        }
        if parts.is_empty() {
            return parts;
        }
        parts.push(MaximalCorePiece { start: words[first].start, end: piece.end, first_word: first, ..piece });
        parts
    }

    /// A start term's words before its first seam (from_start), or an end term's after its last: a term that ends or
    /// begins at the word boundary a hyphen or a sentence's end makes, with no seam in it.
    fn seamless_part(&self, term: &LexicalTerm, from_start: bool) -> Option<LexicalTerm> {
        let words = self.replay.document.tokens();
        let hyphen_after = |index: usize| has_seam(self.replay.document.slice(words[index].start, words[index + 1].end));
        let (start, end, first_word, last_word) = if from_start {
            let cut = (term.first_word..term.last_word).find(|&index| hyphen_after(index))?;
            (term.start, words[cut].end, term.first_word, cut)
        } else {
            let cut = (term.first_word..term.last_word).rev().find(|&index| hyphen_after(index))?;
            (words[cut + 1].start, term.end, cut + 1, term.last_word)
        };
        let text = self.replay.fragment_spelling(self.replay.document.slice(start, end));
        (!has_seam(&text)).then_some(LexicalTerm { text, start, end, first_word, last_word, form: LexicalForm::Run })
    }

    /// `term` without the quotation marks it opens or closes on, where any remain and none are left inside it.
    fn quote_free(&self, term: &LexicalTerm) -> Option<LexicalTerm> {
        let quote = |character: char| has_quote_mark(character.encode_utf8(&mut [0; 4]));
        let source = self.replay.document.slice(term.start, term.end);
        let opened = source.trim_start_matches(quote);
        let inner = opened.trim_end_matches(quote);
        if inner.len() == source.len() || inner.trim().is_empty() {
            return None;
        }
        let start = term.start + utf16_len(&source[..source.len() - opened.len()]);
        let end = start + utf16_len(inner);
        let text = self.replay.fragment_spelling(inner);
        (!has_seam(&text)).then_some(LexicalTerm {
            text, start, end, first_word: term.first_word, last_word: term.last_word, form: LexicalForm::Run,
        })
    }

    fn range_directive_for(
        &mut self,
        piece: MaximalCorePiece,
        head_piece: MaximalCorePiece,
        tail_piece: MaximalCorePiece,
        desired: PhraseSpan,
        required: &HashSet<usize>,
        require_multiword_endpoints: bool,
        allow_ordered_start: bool,
    ) -> Option<String> {
        self.range_directives_for(piece, head_piece, tail_piece, desired, required, require_multiword_endpoints,
            allow_ordered_start, false).map(|(shortest, preferred)| preferred.unwrap_or(shortest))
    }

    /// The shortest range directive, and the one preferred to it where that one has a one-word endpoint: one whose
    /// endpoints have three words or more, else two.
    fn range_directives_for(
        &mut self,
        piece: MaximalCorePiece,
        head_piece: MaximalCorePiece,
        tail_piece: MaximalCorePiece,
        desired: PhraseSpan,
        required: &HashSet<usize>,
        require_multiword_endpoints: bool,
        allow_ordered_start: bool,
        seamed_contexts: bool,
    ) -> Option<(String, Option<String>)> {
        if piece.first_word >= piece.last_word {
            return None;
        }

        let mut heads = Vec::new();
        for term in self.endpoint_terms_from_start(head_piece, false) {
            if term.first_word != piece.first_word {
                continue;
            }
            if require_multiword_endpoints && term.first_word == term.last_word {
                continue;
            }
            heads.push(term);
        }
        let mut tails = Vec::new();
        for term in self.endpoint_terms_from_end(tail_piece, false) {
            if term.last_word != piece.last_word {
                continue;
            }
            if require_multiword_endpoints && term.first_word == term.last_word {
                continue;
            }
            tails.push(term);
        }
        // Each endpoint with a seam cut at the word boundary its first seam (a start) or last seam (an end) makes, and
        // each that opens or closes on a quotation mark read without it: no term holds a seam.
        for from_start in [true, false] {
            let terms = if from_start { &heads } else { &tails };
            let variants = terms.iter()
                .flat_map(|term| [self.seamless_part(term, from_start), self.quote_free(term)])
                .flatten()
                .filter(|term| !require_multiword_endpoints || term.first_word < term.last_word)
                .collect::<Vec<_>>();
            let terms = if from_start { &mut heads } else { &mut tails };
            terms.retain(|term| !self.seamed(&term.text));
            for term in variants {
                if !terms.iter().any(|other| other.start == term.start && other.end == term.end) {
                    terms.push(term);
                }
            }
            // From the piece's edge inward, shortest first.
            let reach = |term: &LexicalTerm| term.last_word - term.first_word;
            terms.sort_by_key(reach);
            // An endpoint longer than the first that the document holds once, of three words or more, only lengthens
            // a directive: each pairing it is in, that one makes shorter and as surely. A paragraph's passage offers
            // an endpoint for each of its words up to a seam.
            if let Some(unique) = terms.iter().position(|term| reach(term) >= 2
                && self.replay_exact_text(&term.text, "", "").count == 1)
            {
                let longest = reach(&terms[unique]);
                terms.retain(|term| reach(term) <= longest);
            }
        }
        if heads.is_empty() || tails.is_empty() {
            return None;
        }

        let words = self.replay.document.tokens();
        let labelled = self.labelled_opening(piece);
        let mut prefixes = if piece.first_word > piece.context_first_word {
            let context = MaximalCorePiece {
                start: words[piece.context_first_word].start,
                end: words[piece.first_word - 1].end,
                first_word: piece.context_first_word,
                last_word: piece.first_word - 1,
                context_first_word: piece.context_first_word,
                context_last_word: piece.first_word - 1,
            };
            let terms = self.endpoint_terms_from_end(context, true);
            self.shortest_sufficient_context(terms, desired, required)
        } else {
            Vec::new()
        };
        let mut suffixes = if piece.last_word < piece.context_last_word {
            let context = MaximalCorePiece {
                start: words[piece.last_word + 1].start,
                end: words[piece.context_last_word].end,
                first_word: piece.last_word + 1,
                last_word: piece.context_last_word,
                context_first_word: piece.last_word + 1,
                context_last_word: piece.context_last_word,
            };
            let terms = self.endpoint_terms_from_start(context, true);
            self.shortest_sufficient_context(terms, desired, required)
        } else {
            Vec::new()
        };
        // As atomic_directive_for: a context meeting the piece at a seam is read only where no other tells it apart.
        let dropped = !seamed_contexts
            && (prefixes.iter().any(|term| self.boundary_seam(term.last_word, piece.first_word))
                || suffixes.iter().any(|term| self.boundary_seam(piece.last_word, term.first_word)));
        if !seamed_contexts {
            prefixes.retain(|term| !self.boundary_seam(term.last_word, piece.first_word));
            suffixes.retain(|term| !self.boundary_seam(piece.last_word, term.first_word));
        }
        // As atomic_directive_for: on a page that may repeat the passage, a range inside its paragraph takes a prefix
        // where it has one, of two words or more where the prefix has them.
        let guarded = labelled || self.repeating_page && !self.pdf && !self.source_page
            && piece.first_word > piece.context_first_word && !prefixes.is_empty();
        if guarded && !labelled {
            let longer = prefixes.iter().filter(|term| term.last_word > term.first_word).cloned().collect::<Vec<_>>();
            if !longer.is_empty() {
                prefixes = longer;
            }
        }
        // For each tail: None when its spelling has no browser term, otherwise the last word
        // between the earliest head end and the tail where the tail's term also matches. A
        // pairing is rejected exactly when that word falls after its head, which is what scanning
        // every word between each head and tail decided, once per pairing.
        let earliest = heads
            .iter()
            .map(|head| head.last_word + 1)
            .min()
            .unwrap_or(0);
        // Only a position holding the term's first word can match it, so the document's positions
        // of that word, last first, are the candidates.
        let tail_earlier = tails
            .iter()
            .map(|tail| {
                let term = self.replay.spelled_term(&tail.text)?;
                let positions = term
                    .words
                    .first()
                    .and_then(|word| self.replay.index.postings.get(&search::word_hash(word)));
                Some(positions.and_then(|positions| {
                    positions
                        .iter()
                        .rev()
                        .copied()
                        .filter(|&word| word >= earliest && word < tail.first_word)
                        .find(|&word| self.replay.spelled_term_at(&term, word, false).is_some())
                }))
            })
            .collect::<Vec<_>>();
        // A directive is "text=" + prefix context + start + "," + end + suffix context, so its
        // length is the sum of its once-encoded parts; a candidate that cannot be shorter than
        // the one found is skipped without spelling the directive out.
        let head_lengths = heads
            .iter()
            .map(|head| encoded_fragment(&head.text).len())
            .collect::<Vec<_>>();
        let tail_lengths = tails
            .iter()
            .map(|tail| encoded_fragment(&tail.text).len())
            .collect::<Vec<_>>();
        // A one-word endpoint: page text the provider's copy lacks (a title, a table of contents, a marginal note) may
        // hold the word before the passage, so a browser starts or ends the range there.
        let single = |term: &LexicalTerm| term.first_word == term.last_word;
        // A short endpoint: two words a page's own text may hold before the passage just as one word may.
        let short = |term: &LexicalTerm| term.last_word - term.first_word + 1 < 3;
        // Encoded lengths of the contexts.
        let prefix_lengths = prefixes.iter().map(|term| encoded_context(&term.text, true).len()).collect::<Vec<_>>();
        let suffix_lengths = suffixes.iter().map(|term| encoded_context(&term.text, false).len()).collect::<Vec<_>>();
        // The shortest directive the pairings select: (strictness 0) of any endpoints; (2) only of endpoints of more
        // than one word; (3) only of endpoints of three words or more. With whether it has a one-word endpoint. No
        // context is tried before one-sided ones, and those before two-sided ones.
        let search = |strictness: u8| -> Option<(String, bool)> {
        // Whether the pairing may be tried, and whether its start is read in order (no uniqueness asked of it).
        let pairing = |head_index: usize, tail_index: usize| -> Option<bool> {
            let (head, tail) = (&heads[head_index], &tails[tail_index]);
            if head.last_word >= tail.first_word
                || (strictness > 1 && (single(head) || single(tail)))
                || (strictness > 2 && (short(head) || short(tail)))
            {
                return None;
            }
            let ordered_start = allow_ordered_start && !self.pdf && head.last_word - head.first_word + 1 >= 3;
            if !ordered_start {
                // Chromium would end the range at an earlier copy of the tail between
                // the head and this tail; the last such copy is the one that decides it.
                let earlier = tail_earlier[tail_index]?;
                if earlier.is_some_and(|word| word > head.last_word) {
                    return None;
                }
            }
            Some(ordered_start)
        };
        // Contexts are read from the piece outward, each one word longer than the one before, so a longer one only
        // drops matches a shorter one makes.
        let (prefix_count, suffix_count) = (prefixes.len(), suffixes.len());
        // An endpoint replays the same way for every partner, so its
        // uniqueness under a context is decided once per endpoint and
        // context rather than once per head and tail pairing. Measured on
        // a Bhasin sentence the judgment repeats: 84,000 replays and 10 s
        // for one plan before this, since every head was re-replayed for
        // every tail and the reverse.
        let mut unique_heads = HashMap::<(usize, Option<usize>, usize), bool>::new();
        let mut unique_tails = HashMap::<(usize, Option<usize>, usize), bool>::new();
        fn context_text(terms: &[LexicalTerm], index: Option<usize>) -> &str {
            index.map_or("", |index| terms[index].text.as_str())
        }
        for sides in 0..3 {
            let mut shortest = None::<(String, bool)>;
            for head_index in 0..heads.len() {
                for tail_index in 0..tails.len() {
                    let Some(ordered_start) = pairing(head_index, tail_index) else {
                        continue;
                    };
                    let (head, tail) = (&heads[head_index], &tails[tail_index]);
                    let endpoints = "text=,".len() + head_lengths[head_index] + tail_lengths[tail_index];
                    let length = |prefix: Option<usize>, suffix: Option<usize>| endpoints
                        + prefix.map_or(0, |index| prefix_lengths[index]) + suffix.map_or(0, |index| suffix_lengths[index]);
                    // Whether the pairing under these contexts selects the piece; the shortest such kept.
                    let mut try_contexts = |prefix: Option<usize>, suffix: Option<usize>,
                                            shortest: &mut Option<(String, bool)>| {
                        let (prefix_text, suffix_text) = (context_text(&prefixes, prefix), context_text(&suffixes, suffix));
                        // The endpoints without their outer marks where those select (a range's middle meets no
                        // context, so the start's end and the end's start shed theirs too), else with them.
                        let shed = (shed_outer_punctuation(&head.text, prefix.is_none(), true),
                            shed_outer_punctuation(&tail.text, true, suffix.is_none()));
                        let marked = (head.text.clone(), tail.text.clone());
                        let variants = if shed == marked { vec![shed] } else { vec![shed, marked] };
                        for (variant, (head_text, tail_text)) in variants.iter().enumerate() {
                            if !ordered_start {
                                let head_unique = *unique_heads.entry((head_index, prefix, variant)).or_insert_with(|| {
                                    self.replay_exact_text(head_text, prefix_text, "").count == 1
                                });
                                if !head_unique {
                                    continue;
                                }
                                let tail_unique = *unique_tails.entry((tail_index, suffix, variant)).or_insert_with(|| {
                                    self.replay_exact_text(tail_text, "", suffix_text).count == 1
                                });
                                if !tail_unique {
                                    continue;
                                }
                            }
                            if !selects(piece, self.replay_range_text(head_text, tail_text, prefix_text, suffix_text)) {
                                continue;
                            }
                            if shortest.as_ref().is_none_or(|(current, _)| length(prefix, suffix) < current.len()) {
                                *shortest = Some((
                                    text_range_directive_with_context(head_text, tail_text, prefix_text, suffix_text),
                                    single(head) || single(tail)));
                            }
                            return true;
                        }
                        false
                    };
                    // Only a shorter directive can replace the one already found, so a one-sided or bare pairing that
                    // cannot be shorter is never replayed.
                    let shorter = |shortest: &Option<(String, bool)>, prefix: Option<usize>, suffix: Option<usize>|
                        shortest.as_ref().is_none_or(|(current, _)| length(prefix, suffix) < current.len());
                    match sides {
                        // A guarded piece is told apart by its prefix: never without one.
                        0 if guarded => {}
                        0 => {
                            if shorter(&shortest, None, None) {
                                try_contexts(None, None, &mut shortest);
                            }
                        }
                        1 => {
                            let one_sided = (0..prefix_count).map(|index| (Some(index), None))
                                .chain((0..if guarded { 0 } else { suffix_count }).map(|index| (None, Some(index))));
                            for (prefix, suffix) in one_sided {
                                if shorter(&shortest, prefix, suffix) {
                                    try_contexts(prefix, suffix, &mut shortest);
                                }
                            }
                        }
                        _ => {
                            // The shortest suffix that selects with each prefix is no longer than with the prefix
                            // before it, so the pairings worth reading form a staircase: each prefix in turn, its
                            // suffix shortened while the pairing still selects.
                            let mut suffix = suffix_count;
                            for prefix in 0..prefix_count {
                                if suffix_count == 0 || !shorter(&shortest, Some(prefix), Some(0)) {
                                    break;
                                }
                                while suffix > 0 && try_contexts(Some(prefix), Some(suffix - 1), &mut shortest) {
                                    suffix -= 1;
                                }
                            }
                        }
                    }
                }
            }
            if shortest.is_some() {
                return shortest;
            }
        }
        None
        };
        // One with endpoints of three words or more is preferred; else, where the one found has a one-word endpoint,
        // one with endpoints of two words or more.
        let Some((found, single_endpoint)) = search(0) else {
            return if dropped {
                self.range_directives_for(piece, head_piece, tail_piece, desired, required, require_multiword_endpoints,
                    allow_ordered_start, true)
            } else {
                None
            };
        };
        let preferred = search(3).or_else(|| if single_endpoint { search(2) } else { None })
            .map(|(directive, _)| directive).filter(|directive| *directive != found);
        Some((found, preferred))
    }

    fn full_document_edge_range(
        &mut self,
        piece: MaximalCorePiece,
        desired: PhraseSpan,
        required: &HashSet<usize>,
    ) -> Option<String> {
        if self.pdf || piece.first_word >= piece.last_word {
            return None;
        }
        let words = self.replay.document.tokens();
        let head = self.atom_at(piece.first_word)?;
        if head.first_word != piece.first_word || head.last_word != piece.first_word {
            return None;
        }
        let mut tails = Vec::new();
        for width in 1..=4.min(piece.last_word - piece.first_word) {
            let first_word = piece.last_word + 1 - width;
            let text = self.replay.fragment_spelling(
                self.replay
                    .document
                    .slice(words[first_word].start, words[piece.last_word].end),
            );
            if !text.contains(". ") && !text.contains("! ") && !text.contains("? ") {
                tails.push(text);
            }
        }
        let context = |from: usize, to: usize| LexicalTerm {
            text: self
                .replay
                .fragment_spelling(self.replay.document.slice(words[from].start, words[to].end)),
            start: words[from].start,
            end: words[to].end,
            first_word: from,
            last_word: to,
            form: LexicalForm::Run,
        };
        let mut prefixes = Vec::new();
        for width in 1..=8.min(piece.first_word) {
            let term = context(piece.first_word - width, piece.first_word - 1);
            if self.context_allowed(&term, desired, required) {
                prefixes.push(term.text);
            }
        }
        let mut suffixes = Vec::new();
        for width in 1..=8.min(words.len() - piece.last_word - 1) {
            let term = context(piece.last_word + 1, piece.last_word + width);
            if self.context_allowed(&term, desired, required) {
                suffixes.push(term.text);
            }
        }
        prefixes.push(String::new());
        suffixes.push(String::new());
        let mut shortest = None::<String>;
        for tail in tails {
            for prefix in &prefixes {
                for suffix in &suffixes {
                    if !selects(
                        piece,
                        self.replay_range_text(&head.text, &tail, prefix, suffix),
                    ) {
                        continue;
                    }
                    let directive =
                        text_range_directive_with_context(&head.text, &tail, prefix, suffix);
                    if shortest
                        .as_ref()
                        .is_none_or(|current| directive.len() < current.len())
                    {
                        shortest = Some(directive);
                    }
                }
            }
        }
        shortest
    }

    fn bare_atomic_directive(&mut self, piece: MaximalCorePiece) -> Option<String> {
        let target = self.exact_target_for(piece).filter(|target| !self.seamed(&target.text))?;
        let replay = self.replay_exact_text(&target.text, "", "");
        (replay.count == 1 && selects(piece, replay.first)).then(|| text_directive(&target.text, "", ""))
    }

    fn full_exact_directive_across_lines(&mut self, piece: MaximalCorePiece) -> Option<String> {
        let words = self.replay.document.tokens();
        let target = self.replay.fragment_spelling(
            self.replay
                .document
                .slice(words[piece.first_word].start, words[piece.last_word].end),
        );
        let target_term = self.replay.spelled_term(&target)?;
        let prefixes = if piece.first_word > piece.context_first_word {
            self.endpoint_terms_from_end(
                MaximalCorePiece {
                    start: words[piece.context_first_word].start,
                    end: words[piece.first_word - 1].end,
                    first_word: piece.context_first_word,
                    last_word: piece.first_word - 1,
                    context_first_word: piece.context_first_word,
                    context_last_word: piece.first_word - 1,
                },
                true,
            )
        } else {
            Vec::new()
        };
        let suffixes = if piece.last_word < piece.context_last_word {
            self.endpoint_terms_from_start(
                MaximalCorePiece {
                    start: words[piece.last_word + 1].start,
                    end: words[piece.context_last_word].end,
                    first_word: piece.last_word + 1,
                    last_word: piece.context_last_word,
                    context_first_word: piece.last_word + 1,
                    context_last_word: piece.context_last_word,
                },
                true,
            )
        } else {
            Vec::new()
        };
        // The shortest directive that selects the piece, of no context, each prefix, each suffix, then each pair, the
        // first offered where lengths tie. A context adds its own length to the directive's, so the contexts are
        // replayed shortest first until one selects the piece: a line of hundreds of words offers tens of thousands.
        let (prefixes, suffixes) = ([None].into_iter().chain(prefixes.iter().map(Some)).collect::<Vec<_>>(),
            [None].into_iter().chain(suffixes.iter().map(Some)).collect::<Vec<_>>());
        let length = |term: Option<&LexicalTerm>, prefix: bool| term.map_or(0, |term| encoded_context(&term.text, prefix).len());
        let (prefix_lengths, suffix_lengths) = (prefixes.iter().map(|term| length(*term, true)).collect::<Vec<_>>(),
            suffixes.iter().map(|term| length(*term, false)).collect::<Vec<_>>());
        let (more_prefixes, more_suffixes) = (prefixes.len() - 1, suffixes.len() - 1);
        let mut order = (0..prefixes.len()).flat_map(|at| (0..suffixes.len()).map(move |to| (at, to))).map(|(at, to)| {
            let offered = match (at, to) {
                (at, 0) => at,
                (0, to) => more_prefixes + to,
                (at, to) => more_prefixes + more_suffixes + (at - 1) * more_suffixes + to,
            };
            (prefix_lengths[at] + suffix_lengths[to], offered, at, to)
        }).collect::<Vec<_>>();
        order.sort_unstable();
        order.into_iter().find_map(|(_, _, at, to)| {
            let prefix = prefixes[at].map_or("", |term| term.text.as_str());
            let suffix = suffixes[to].map_or("", |term| term.text.as_str());
            let replay = self.replay.replay_exact(&target_term, prefix, suffix, true);
            (replay.count == 1 && selects(piece, replay.first)).then(|| text_directive(&target, prefix, suffix))
        })
    }

    fn starts_across_sentence_opening_capital(&self, piece: MaximalCorePiece) -> bool {
        // Short selections already have reliable exact/range candidates. This fallback is
        // specifically for long selections whose first endpoint crosses an invisible HTML seam.
        if piece.last_word - piece.first_word + 1 < 64 {
            return false;
        }
        let words = self.replay.document.tokens();
        let gap = self.replay.document.slice(
            words[piece.first_word].end,
            words[piece.first_word + 1].start,
        );
        gap.contains('.')
            && self
                .replay
                .document
                .slice(
                    words[piece.first_word + 1].start,
                    words[piece.first_word + 1].end,
                )
                .chars()
                .next()
                .is_some_and(char::is_uppercase)
    }

    fn candidate_for(
        &self,
        piece: MaximalCorePiece,
        quote_index: usize,
        directive: String,
    ) -> Candidate {
        let words = self.replay.document.tokens();
        let encoded_length = directive.len();
        Candidate {
            piece: BuiltPiece {
                directive,
                quote_index,
                start: words[piece.first_word].start,
                end: words[piece.last_word].end,
                first_word: piece.first_word,
                last_word: piece.last_word,
            },
            encoded_length,
        }
    }

    fn candidate_for_piece(
        &mut self,
        piece: MaximalCorePiece,
        quote_index: usize,
        requested_first: usize,
        requested_last: usize,
        desired: PhraseSpan,
        required: &HashSet<usize>,
    ) -> Option<Candidate> {
        self.atomic_directive_for(
            piece,
            quote_index,
            requested_first,
            requested_last,
            desired,
            required,
        )
        .map(|directive| self.candidate_for(piece, quote_index, directive))
    }

    fn bare_atomic_replays(&mut self, piece: MaximalCorePiece) -> bool {
        self.bare_atomic_directive(piece).is_some()
    }

    fn crosses_lossy_pdf_tight_token_seam(&self, piece: MaximalCorePiece) -> bool {
        static ORDINAL_SUFFIX: OnceLock<Regex> = OnceLock::new();
        let words = self.replay.document.tokens();
        (piece.first_word..piece.last_word).any(|left| {
            let left_text = self
                .replay
                .document
                .slice(words[left].start, words[left].end);
            let right_text = self
                .replay
                .document
                .slice(words[left + 1].start, words[left + 1].end);
            left_text
                .chars()
                .all(|character| character.is_ascii_digit())
                && js_regex(r"(?iu)^(?:st|nd|rd|th)$", &ORDINAL_SUFFIX).is_match(right_text)
                && self
                    .replay
                    .document
                    .slice(words[left].end, words[left + 1].start)
                    .chars()
                    .any(javascript_whitespace)
        })
    }
}

/// Hyphen characters a publisher may print in place of one another.
fn has_hyphen(text: &str) -> bool {
    text.contains(['-', '\u{2010}', '\u{2011}'])
}

/// Where a publisher may set the words differently from the provider's copy, so that a browser finds neither for the
/// other: a hyphen (printed as another hyphen character: CanLII's legislation prints U+2011 where the provider has
/// "-"), and the space after a full stop, question or exclamation mark (set as a no-break space and a space, which a
/// browser reads as two).
/// Quotation marks and primes: a browser folds none of them onto another (curly against straight quotes, low,
/// reversed and guillemet quotes, primes against apostrophes), and a publisher may print any of them for another.
fn has_quote_mark(text: &str) -> bool {
    text.contains(['"', '\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}', '\u{201C}', '\u{201D}', '\u{201E}',
        '\u{201F}', '\u{AB}', '\u{BB}', '\u{2032}', '\u{2033}'])
}

/// Whether a term holds a seam a page may print otherwise than the source: a sentence's end (a page may space it with
/// a no-break space and a space) or a hyphen (U+2011 for `-`). No term holds one; a seam falls only between terms,
/// where a browser takes any spacing. Measured on the cached pages (2026-10-18): a term across a sentence's end
/// painted nothing on 36% of CanLII links and 50% of publisher links, against 3% and 2% with no seam; quotation
/// marks, apostrophes and dashes failed about one in ten, so a term may hold them (endpoints are also offered
/// without the marks they open or close on).
fn has_seam(text: &str) -> bool {
    let words = text.split_whitespace().collect::<Vec<_>>();
    // An ellipsis: a page may space its dots (". . ." for "..."), which a browser does not fold.
    has_hyphen(text) || text.contains(['…']) || text.contains("..") || text.contains(". .")
        || words.windows(2).any(|pair| sentence_end(pair[0], pair[1]) || spaced_apart(pair[0], pair[1]))
}

/// Whether a page may set `word` and `next` apart otherwise than the source (measured on the cached pages,
/// 2026-10-18): a pinpoint's "at" and its "para." (CanLII sets a no-break space and a space), a provision's number
/// and its parenthesized part ("15 (1)", "11 (e)": a page may close them up), and a printed paragraph number and
/// the words after it ("[101]  In": an anchor and spaces).
fn spaced_apart(word: &str, next: &str) -> bool {
    let bare = |value: &str| value.trim_end_matches(['.', ',']).to_lowercase();
    let pinpoint = word.eq_ignore_ascii_case("at")
        && matches!(bare(next).as_str(), "para" | "paras" | "p" | "pp" | "paragraph" | "paragraphs" | "page" | "pages");
    // Read with or without the closing mark a term cut inside the locator leaves off ("15 (1").
    let locator = word.ends_with(|character: char| character.is_ascii_digit() || character == ')')
        && next.strip_prefix('(').map(|rest| rest.split(')').next().unwrap_or(rest)).is_some_and(|part|
            !part.is_empty() && (part.len() <= 3 && part.bytes().all(|byte| byte.is_ascii_digit())
                || part.chars().count() <= 4 && part.chars().all(char::is_alphabetic)));
    // With or without its opening bracket, which a term starting at the number leaves off ("101] In").
    let label = word.trim_start_matches('[').strip_suffix(']')
        .is_some_and(|number| (1..=3).contains(&number.len()) && number.bytes().all(|byte| byte.is_ascii_digit()));
    pinpoint || locator || label
}

/// Abbreviations whose full stop is no sentence's end.
const ABBREVIATIONS: &[&str] = &["Mr", "Ms", "Mrs", "Mme", "Dr", "St", "Jr", "Sr", "Hon", "No", "Nos", "Co", "Inc",
    "Ltd", "Corp", "s", "ss", "p", "pp", "para", "paras", "v", "vs", "c", "ch", "art", "arts", "al", "cf", "ed",
    "eds", "Vol", "vol", "Rev", "Stat", "Reg", "Supp", "fig", "approx", "et"];

/// Whether `word` ends a sentence before `next`: a full stop, question or exclamation mark (inside any closing marks)
/// before a word that opens on a capital, the stop no abbreviation's (a single letter, "U.S.", "Mr.", "s.", "v.").
/// Pages set an abbreviation's stop with one space and often a sentence's with a no-break space and a space: of
/// some 36,000 abbreviation stops on cached SCC and CanLII pages none had the two, against 43-46% of sentence ends.
pub fn sentence_end(word: &str, next: &str) -> bool {
    let opens_capital = next.trim_start_matches(|character: char| !character.is_alphanumeric())
        .chars().next().is_some_and(char::is_uppercase);
    // A bracketed citation after a stop ("Crown. [para. 41]", "disrepute. (R. v. Grant …)") is set apart as a
    // sentence's end is: a seam, whatever case it opens in.
    let opens_citation = next.starts_with(['[', '(']);
    let word = word.trim_end_matches(is_closing);
    if !opens_capital && !opens_citation {
        return false;
    }
    if word.ends_with(['?', '!']) {
        return true;
    }
    let Some(stem) = word.strip_suffix('.') else {
        return false;
    };
    let stem = stem.trim_start_matches(|character: char| !character.is_alphanumeric());
    !(stem.chars().count() <= 1 || stem.contains('.') || ABBREVIATIONS.contains(&stem))
}

/// A closing quotation mark or bracket a sentence may end inside.
fn is_closing(character: char) -> bool {
    matches!(character, '"' | '\'' | '\u{2019}' | '\u{201D}' | ')' | ']' | '\u{BB}')
}

fn better_cover(left: Option<Vec<Candidate>>, right: Vec<Candidate>) -> Vec<Candidate> {
    let Some(left) = left else {
        return right;
    };
    if right.len() != left.len() {
        return if right.len() < left.len() {
            right
        } else {
            left
        };
    }
    let left_length = left.iter().map(|item| item.encoded_length).sum::<usize>();
    let right_length = right.iter().map(|item| item.encoded_length).sum::<usize>();
    if right_length < left_length {
        right
    } else {
        left
    }
}

fn cover_run(
    first_word: usize,
    last_word: usize,
    candidates: &[Candidate],
) -> Option<Vec<Candidate>> {
    let mut best = HashMap::<usize, Vec<Candidate>>::new();
    best.insert(first_word, Vec::new());
    for next_word in first_word..=last_word {
        let Some(prefix) = best.get(&next_word).cloned() else {
            continue;
        };
        for candidate in candidates {
            if candidate.piece.first_word > next_word
                || candidate.piece.last_word < next_word
                || candidate.piece.first_word < first_word
                || candidate.piece.last_word > last_word
            {
                continue;
            }
            let after = candidate.piece.last_word + 1;
            let mut cover = prefix.clone();
            cover.push(candidate.clone());
            let previous = best.remove(&after);
            best.insert(after, better_cover(previous, cover));
        }
    }
    best.remove(&(last_word + 1))
}

#[derive(Clone, Default)]
struct RangeRepair {
    candidates: Vec<Candidate>,
    painted_words: usize,
    pinned_overlap_words: usize,
    encoded_length: usize,
}

fn better_range_repair(left: Option<RangeRepair>, right: RangeRepair) -> RangeRepair {
    let Some(left) = left else {
        return right;
    };
    let left_score = (
        left.painted_words,
        left.pinned_overlap_words,
        left.candidates.len(),
        left.encoded_length,
    );
    let right_score = (
        right.painted_words,
        right.pinned_overlap_words,
        right.candidates.len(),
        right.encoded_length,
    );
    if right_score < left_score {
        right
    } else {
        left
    }
}

fn repair_with_ranges(
    needed_words: &HashSet<usize>,
    pinned_words: &HashSet<usize>,
    candidates: &[Candidate],
) -> Option<Vec<Candidate>> {
    let mut needed = needed_words.iter().copied().collect::<Vec<_>>();
    needed.sort_unstable();
    if needed.is_empty() {
        return Some(Vec::new());
    }

    let mut best = HashMap::<usize, RangeRepair>::new();
    best.insert(0, RangeRepair::default());
    for needed_index in 0..needed.len() {
        let Some(prefix) = best.get(&needed_index).cloned() else {
            continue;
        };
        let word = needed[needed_index];
        for candidate in candidates {
            if candidate.piece.first_word > word || candidate.piece.last_word < word {
                continue;
            }
            let after =
                needed.partition_point(|needed_word| *needed_word <= candidate.piece.last_word);
            let mut repair = prefix.clone();
            repair.painted_words += candidate.piece.last_word - candidate.piece.first_word + 1;
            repair.pinned_overlap_words += (candidate.piece.first_word..=candidate.piece.last_word)
                .filter(|painted| pinned_words.contains(painted))
                .count();
            repair.encoded_length += candidate.encoded_length;
            repair.candidates.push(candidate.clone());
            let previous = best.remove(&after);
            best.insert(after, better_range_repair(previous, repair));
        }
    }
    best.remove(&needed.len()).map(|repair| repair.candidates)
}

fn duplicate_signature_metadata(document: FragmentText<'_>, desired: PhraseSpan) -> HashSet<usize> {
    static SIGNATURE: OnceLock<Regex> = OnceLock::new();
    static FORMAL_CITATION: OnceLock<Regex> = OnceLock::new();
    let words = document.tokens();
    let mut citation = desired.first_word;
    while citation <= desired.last_word
        && !document
            .slice(words[citation].start, words[citation].end)
            .eq_ignore_ascii_case("citation")
    {
        citation += 1;
    }
    if citation == desired.first_word || citation > desired.last_word {
        return HashSet::new();
    }
    let signature_words = words[desired.first_word..citation]
        .iter()
        .map(|word| document.slice(word.start, word.end).to_lowercase())
        .collect::<Vec<_>>();
    let signature = signature_words.join(" ");
    if !js_regex(
        r"(?iu)^at\b.+\bthis\s+\d+(?:st|nd|rd|th)\s+day\s+of\s+.+\s+\d{4}\b.+\bj$",
        &SIGNATURE,
    )
    .is_match(&signature)
        || document
            .phrase_spans(
                &signature_words,
                PhraseOptions {
                    limit: Some(2),
                    ..Default::default()
                },
            )
            .len()
            < 2
    {
        return HashSet::new();
    }
    let formal_citation = words[citation + 1..(citation + 4).min(desired.last_word + 1)]
        .iter()
        .map(|word| document.slice(word.start, word.end).to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let through_heading = if js_regex(r"(?iu)^\d{4}\s+[a-z]{2,8}\s+\d+$", &FORMAL_CITATION)
        .is_match(&formal_citation)
    {
        citation + 1
    } else {
        citation
    };
    (desired.first_word..through_heading).collect()
}

fn required_runs(desired: PhraseSpan, required: &HashSet<usize>) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut first = None;
    for word in desired.first_word..=desired.last_word + 1 {
        if word <= desired.last_word && required.contains(&word) {
            first.get_or_insert(word);
            continue;
        }
        if let Some(start) = first.take() {
            runs.push((start, word - 1));
        }
    }
    runs
}

fn crosses_multiple_lettered_breaks(document: FragmentText<'_>, plan: &TextFragmentPlan) -> bool {
    let mut quote_spans = HashMap::<usize, (usize, usize)>::new();
    for interval in &plan.source_word_intervals {
        let span = quote_spans
            .entry(interval.quote_index)
            .or_insert((interval.start, interval.end));
        span.0 = span.0.min(interval.start);
        span.1 = span.1.max(interval.end);
    }
    quote_spans.into_values().any(|(start, end)| {
        document
            .slice(start, end)
            .split(['\r', '\n'])
            .skip(1)
            .filter(|line| {
                let bytes = line.trim_start().as_bytes();
                bytes.len() >= 3
                    && bytes[0] == b'('
                    && bytes[1].is_ascii_lowercase()
                    && bytes[2] == b')'
            })
            .take(2)
            .count()
            == 2
    })
}

fn build_fragment_plan(
    block_text: &str,
    document: FragmentText<'_>,
    quotes: &[String],
    pdf: bool,
    publisher_may_annotate_legal_reference: bool,
    split_html_source_blocks: bool,
    source_page: bool,
    repeating_page: bool,
) -> TextFragmentPlan {
    // A block that is its document's whole text reads the document's index, not one built again.
    let block_search;
    let block = match document {
        FragmentText::Text(search) if search.text.value == block_text => document,
        _ => {
            block_search = TextSearch::new(block_text);
            FragmentText::Text(&block_search)
        }
    };
    let mut selections = Vec::new();
    let mut keys = HashSet::new();
    let mut complete = true;

    for (quote_index, quote) in quotes.iter().enumerate() {
        let quote_words = fragment_words(quote);
        let key = quote_words.join(" ");
        if key.is_empty() || !keys.insert(key) {
            continue;
        }
        let Some(selected) = choose_source_span(block, quote, &quote_words) else {
            complete = false;
            continue;
        };
        let mut selection = trimmed_selection(block, quote_index, selected, split_html_source_blocks);
        let Some(desired) = locate_document_quote(block, document, selection.desired) else {
            complete = false;
            continue;
        };
        selection.desired = desired;
        selections.push(selection);
    }
    build_selected_fragment_plan(document, &selections, pdf,
        publisher_may_annotate_legal_reference, split_html_source_blocks, false, complete, source_page, repeating_page)
}

/// `selected` of `text` without its printed label ("[85]", "(a)") or trailing citation artifact, which a publisher
/// may set apart from the words: quotations and passages a locator names are painted from their first word.
fn trimmed_selection(text: FragmentText<'_>, quote_index: usize, selected: PhraseSpan, markdown_labels: bool)
    -> FragmentSelection {
    let desired = adjust_span_edges_with_options(text, selected, true, markdown_labels);
    FragmentSelection { quote_index, desired,
        clamp_leading: desired.start > selected.start,
        clamp_trailing: desired.end < selected.end }
}

struct FragmentSelection {
    quote_index: usize,
    desired: PhraseSpan,
    clamp_leading: bool,
    clamp_trailing: bool,
}

fn build_selected_fragment_plan(
    document: FragmentText<'_>,
    selections: &[FragmentSelection],
    pdf: bool,
    publisher_may_annotate_legal_reference: bool,
    split_html_source_blocks: bool,
    exact_members: bool,
    mut complete: bool,
    source_page: bool,
    repeating_page: bool,
) -> TextFragmentPlan {
    let replay = BrowserReplay::new(document, split_html_source_blocks);
    let mut planner = MaximalPlanner::new(
        &replay,
        pdf,
        publisher_may_annotate_legal_reference,
        split_html_source_blocks,
        source_page,
        repeating_page,
    );
    let source_words = document.tokens();
    let mut built = Vec::<BuiltPiece>::new();

    for selection in selections {
        let FragmentSelection { quote_index, desired, clamp_leading, clamp_trailing } = *selection;
        let line_pieces = planner.source_pieces(desired, clamp_leading, clamp_trailing, exact_members);
        let mut required = line_pieces
            .iter()
            .flat_map(|piece| piece.first_word..=piece.last_word)
            .collect::<HashSet<_>>();
        if !exact_members {
            for word in duplicate_signature_metadata(document, desired) {
                required.remove(&word);
            }
        }

        for (first_word, last_word) in required_runs(desired, &required) {
            let whole = planner.core_piece(
                PhraseSpan {
                    start: source_words[first_word].start,
                    end: source_words[last_word].end,
                    first_word,
                    last_word,
                },
                clamp_leading && first_word == desired.first_word,
                clamp_trailing && last_word == desired.last_word,
            );
            let head_piece = line_pieces
                .iter()
                .copied()
                .find(|piece| piece.first_word <= first_word && piece.last_word >= first_word);
            let tail_piece = line_pieces
                .iter()
                .copied()
                .find(|piece| piece.first_word <= last_word && piece.last_word >= last_word);
            let range_pieces =
                if let (Some(mut head_piece), Some(mut tail_piece)) = (head_piece, tail_piece) {
                    head_piece.first_word = first_word;
                    head_piece.last_word = head_piece.last_word.min(last_word);
                    head_piece.start = source_words[first_word].start;
                    head_piece.end = source_words[head_piece.last_word].end;
                    tail_piece.first_word = tail_piece.first_word.max(first_word);
                    tail_piece.last_word = last_word;
                    tail_piece.start = source_words[tail_piece.first_word].start;
                    tail_piece.end = source_words[last_word].end;
                    Some((head_piece, tail_piece))
                } else {
                    None
                };
            let whole_stays_in_structural_piece = line_pieces
                .iter()
                .any(|piece| piece.first_word <= first_word && piece.last_word >= last_word);
            let separate_sentence_blocks = split_html_source_blocks && line_pieces.len() == 2 && {
                let head = line_pieces[0];
                let tail = line_pieces[1];
                let gap = document.slice(
                    source_words[head.last_word].end,
                    source_words[tail.first_word].start,
                );
                gap.contains(".\n") || gap.contains(".\r\n")
            };
            if split_html_source_blocks
                && whole_stays_in_structural_piece
                && first_word < last_word
                && last_word - first_word < 32
            {
                if let Some(directive) = planner.bare_atomic_directive(whole).or_else(|| {
                    planner.atomic_directive_for(whole, quote_index, first_word, last_word, desired, &required)
                }) {
                    built.push(planner.candidate_for(whole, quote_index, directive).piece);
                    continue;
                }
            }
            if split_html_source_blocks
                && !whole_stays_in_structural_piece
                && !separate_sentence_blocks
                && !(line_pieces.len() == 3 && clamp_leading && last_word - first_word < 15)
            {
                let across_blocks = range_pieces.and_then(|(head, tail)| {
                    planner.range_directive_for(whole, head, tail, desired, &required, false, true)
                });
                if let Some(directive) = across_blocks {
                    built.push(planner.candidate_for(whole, quote_index, directive).piece);
                    continue;
                }
            }
            if !pdf && (!split_html_source_blocks || whole_stays_in_structural_piece) {
                if planner.starts_across_sentence_opening_capital(whole) {
                    if let Some(directive) =
                        planner.full_document_edge_range(whole, desired, &required)
                    {
                        built.push(planner.candidate_for(whole, quote_index, directive).piece);
                        continue;
                    }
                }
                // A short piece with no seam in it reads the same on any page, so its exact directive is as sure as a
                // range and asks every word of it, not two ends a page's other text (a summary, a headnote) may share.
                // Measured on the cached pages (2026-10-18): exact directives of 6-15 words painted on target 98.2%
                // (ranges 98.5%), of 16-30 words 97.1%, of 31 or more 94.6%: a long one meets more of what a page
                // prints between the words (a note number, a link's icon), so it is planned as a range. Up to 30
                // words the exact one stays: a range's two ends may match a page's paraphrase (a CanLII summary).
                if whole_stays_in_structural_piece && whole.last_word - whole.first_word < 30 {
                    if let Some(directive) =
                        planner.atomic_directive_for(whole, quote_index, first_word, last_word, desired, &required)
                    {
                        built.push(planner.candidate_for(whole, quote_index, directive).piece);
                        continue;
                    }
                }
                let conservative = range_pieces.and_then(|(head, tail)| {
                    planner.range_directive_for(whole, head, tail, desired, &required, false, false)
                });
                let ordered = range_pieces.and_then(|(head, tail)| {
                    planner.range_directive_for(whole, head, tail, desired, &required, false, true)
                });
                // Where no range selects the piece, an exact directive of it, or else of it read on to its sentence's
                // edges (back to its paragraph's first words, after the printed label a page sets apart).
                let found = ordered.or(conservative).map(|directive| (whole, directive)).or_else(|| {
                    if !whole_stays_in_structural_piece {
                        return None;
                    }
                    planner.sentence_extensions(whole).into_iter().find_map(|piece| {
                        planner.atomic_directive_for(piece, quote_index, first_word, last_word, desired, &required)
                            .map(|directive| (piece, directive))
                    })
                }).or_else(|| {
                    // Else a range read back to its paragraph's first words, after the printed label a page sets
                    // apart, with that label for context: a passage a headnote repeats is told apart by its number.
                    // Its start is read in order: a browser looks for the end only after the start the label finds,
                    // so the end need not be the document's only copy (the headnote repeats it).
                    let (head, tail) = range_pieces?;
                    let line_first = planner.line_first_word(whole.first_word);
                    let opening = (line_first..whole.first_word)
                        .find(|&word| planner.source_furniture(word) != Some(FurnitureKind::Label))?;
                    if opening == line_first {
                        return None;
                    }
                    let read = |piece: MaximalCorePiece| MaximalCorePiece {
                        start: source_words[opening].start, first_word: opening, context_first_word: line_first, ..piece };
                    let extended = read(whole);
                    planner.range_directive_for(extended, read(head), tail, desired, &required, false, true)
                        .map(|directive| (extended, directive))
                });
                if let Some((piece, directive)) = found {
                    built.push(planner.candidate_for(piece, quote_index, directive).piece);
                    continue;
                }
            }
            if pdf || whole_stays_in_structural_piece {
                if let Some(directive) = planner.atomic_directive_for(
                    whole,
                    quote_index,
                    first_word,
                    last_word,
                    desired,
                    &required,
                ) {
                    if !pdf {
                        built.push(planner.candidate_for(whole, quote_index, directive).piece);
                        continue;
                    }
                    if planner.bare_atomic_replays(whole)
                        && !planner.crosses_lossy_pdf_tight_token_seam(whole)
                    {
                        built.push(planner.candidate_for(whole, quote_index, directive).piece);
                        continue;
                    }
                }
            }
            let mut candidates = Vec::<Candidate>::new();
            let mut risk_words = HashSet::<usize>::new();
            let mut candidate_keys = HashSet::<(usize, usize, String)>::new();
            let add_candidate =
                |candidate: Candidate,
                 candidates: &mut Vec<Candidate>,
                 keys: &mut HashSet<(usize, usize, String)>| {
                    let key = (
                        candidate.piece.first_word,
                        candidate.piece.last_word,
                        candidate.piece.directive.clone(),
                    );
                    if keys.insert(key) {
                        candidates.push(candidate);
                    }
                };

            for line in &line_pieces {
                let piece_first = first_word.max(line.first_word);
                let piece_last = last_word.min(line.last_word);
                if piece_first > piece_last {
                    continue;
                }
                let mut piece = planner.core_piece(
                    PhraseSpan {
                        start: source_words[piece_first].start,
                        end: source_words[piece_last].end,
                        first_word: piece_first,
                        last_word: piece_last,
                    },
                    clamp_leading && piece_first == desired.first_word,
                    clamp_trailing && piece_last == desired.last_word,
                );
                if split_html_source_blocks {
                    if piece_first == line.first_word && piece_first == desired.first_word {
                        piece.context_first_word = line.context_first_word;
                    }
                }
                if let Some(candidate) = planner.candidate_for_piece(
                    piece,
                    quote_index,
                    first_word,
                    last_word,
                    desired,
                    &required,
                ) {
                    let bare_unique = !pdf || planner.bare_atomic_replays(piece);
                    if !pdf || !planner.crosses_lossy_pdf_tight_token_seam(piece) {
                        if pdf && !bare_unique {
                            risk_words.extend(piece.first_word..=piece.last_word);
                        }
                        add_candidate(candidate, &mut candidates, &mut candidate_keys);
                    }
                } else if !pdf {
                    // A piece no one directive selects, read a part between seams at a time (a quotation whose
                    // first sentence a headnote repeats): each part takes the directive it can, the cover the rest.
                    for part in planner.seam_parts(piece) {
                        if let Some(candidate) =
                            planner.candidate_for_piece(part, quote_index, first_word, last_word, desired, &required)
                        {
                            add_candidate(candidate, &mut candidates, &mut candidate_keys);
                        }
                    }
                }
            }

            if !pdf {
                if let Some(cover) = cover_run(first_word, last_word, &candidates) {
                    built.extend(cover.into_iter().map(|candidate| candidate.piece));
                    continue;
                }
            }

            // A duplicated structural piece may be impossible to select alone
            // while remaining safely selectable as the interior of a range
            // between unique neighbouring pieces. Browser replay remains the
            // authority: only ranges resolving to their declared source span
            // become set-cover candidates.
            let mut range_candidates = Vec::<Candidate>::new();
            let mut range_candidate_keys = HashSet::<(usize, usize, String)>::new();
            if pdf {
                if let Some(directive) = range_pieces.and_then(|(head, tail)| {
                    planner.range_directive_for(whole, head, tail, desired, &required, true, false)
                }) {
                    let candidate = planner.candidate_for(whole, quote_index, directive);
                    add_candidate(candidate, &mut range_candidates, &mut range_candidate_keys);
                }
            }
            // The range from one line's first word in the piece to another's last.
            let line_range = |planner: &mut MaximalPlanner<'_, '_>, head: MaximalCorePiece, tail: MaximalCorePiece| {
                let candidate_first = first_word.max(head.first_word);
                let candidate_last = last_word.min(tail.last_word);
                if candidate_first >= candidate_last {
                    return None;
                }
                let span = planner.core_piece(
                    PhraseSpan {
                        start: source_words[candidate_first].start,
                        end: source_words[candidate_last].end,
                        first_word: candidate_first,
                        last_word: candidate_last,
                    },
                    clamp_leading && candidate_first == desired.first_word,
                    clamp_trailing && candidate_last == desired.last_word,
                );
                let mut range_head = head;
                range_head.first_word = candidate_first;
                range_head.start = source_words[candidate_first].start;
                let mut range_tail = tail;
                range_tail.last_word = candidate_last;
                range_tail.end = source_words[candidate_last].end;
                planner.range_directive_for(span, range_head, range_tail, desired, &required, pdf, !pdf)
                    .map(|directive| planner.candidate_for(span, quote_index, directive))
            };
            let lines = line_pieces.iter().copied()
                .filter(|line| line.first_word <= last_word && line.last_word >= first_word)
                .collect::<Vec<_>>();
            if pdf {
                for (head_index, head) in lines.iter().copied().enumerate() {
                    for tail in lines.iter().copied().skip(head_index + 1) {
                        if let Some(candidate) = line_range(&mut planner, head, tail) {
                            add_candidate(candidate, &mut range_candidates, &mut range_candidate_keys);
                        }
                    }
                }
            } else if !split_html_source_blocks && !lines.is_empty() {
                // Ranges only over the lines no line's own directive covers: each run of them, widened a line at a
                // time to either side until a range selects it. Every pairing of lines would cost the square of their
                // number (a passage of hundreds of lines in a judgment of millions of characters).
                let open = lines.iter().map(|line| {
                    let (from, to) = (first_word.max(line.first_word), last_word.min(line.last_word));
                    !candidates.iter().any(|candidate| candidate.piece.first_word <= from && candidate.piece.last_word >= to)
                }).collect::<Vec<_>>();
                let last_line = lines.len() - 1;
                let mut index = 0;
                while index <= last_line {
                    if !open[index] {
                        index += 1;
                        continue;
                    }
                    let first_open = index;
                    while index < last_line && open[index + 1] {
                        index += 1;
                    }
                    for widening in 0..=last_line {
                        let (head, tail) = (first_open.saturating_sub(widening), (index + widening).min(last_line));
                        if let Some(candidate) = line_range(&mut planner, lines[head], lines[tail]) {
                            add_candidate(candidate, &mut range_candidates, &mut range_candidate_keys);
                            break;
                        }
                        if head == 0 && tail == last_line {
                            break;
                        }
                    }
                    index += 1;
                }
            }

            if pdf {
                let pinned_words = candidates
                    .iter()
                    .flat_map(|candidate| candidate.piece.first_word..=candidate.piece.last_word)
                    .collect::<HashSet<_>>();
                let needed_words = (first_word..=last_word)
                    .filter(|word| !pinned_words.contains(word))
                    .collect::<HashSet<_>>();
                let range_coverable = range_candidates
                    .iter()
                    .flat_map(|candidate| candidate.piece.first_word..=candidate.piece.last_word)
                    .collect::<HashSet<_>>();
                let mut repair_words = needed_words.clone();
                repair_words.extend(
                    risk_words
                        .iter()
                        .filter(|word| range_coverable.contains(word))
                        .copied(),
                );
                let repair = repair_with_ranges(&repair_words, &pinned_words, &range_candidates);
                if needed_words
                    .iter()
                    .all(|word| range_coverable.contains(word))
                    && repair.is_some()
                {
                    built.extend(candidates.into_iter().map(|candidate| candidate.piece));
                    built.extend(
                        repair
                            .unwrap_or_default()
                            .into_iter()
                            .map(|candidate| candidate.piece),
                    );
                    continue;
                }

                if let Some(directive) = planner.full_exact_directive_across_lines(whole) {
                    built.push(planner.candidate_for(whole, quote_index, directive).piece);
                    continue;
                }

                complete = false;
                built.extend(candidates.into_iter().map(|candidate| candidate.piece));
                let coverable = repair_words
                    .iter()
                    .filter(|word| range_coverable.contains(word))
                    .copied()
                    .collect::<HashSet<_>>();
                if let Some(partial) =
                    repair_with_ranges(&coverable, &pinned_words, &range_candidates)
                {
                    built.extend(partial.into_iter().map(|candidate| candidate.piece));
                }
                continue;
            }

            for candidate in range_candidates {
                add_candidate(candidate, &mut candidates, &mut candidate_keys);
            }
            if let Some(cover) = cover_run(first_word, last_word, &candidates) {
                built.extend(cover.into_iter().map(|candidate| candidate.piece));
                continue;
            }

            complete = false;
            let coverable = candidates
                .iter()
                .flat_map(|candidate| candidate.piece.first_word..=candidate.piece.last_word)
                .collect::<HashSet<_>>();
            let mut component_first = None;
            for word in first_word..=last_word + 1 {
                if word <= last_word && coverable.contains(&word) {
                    component_first.get_or_insert(word);
                    continue;
                }
                if let Some(start) = component_first.take() {
                    if let Some(partial) = cover_run(start, word - 1, &candidates) {
                        built.extend(partial.into_iter().map(|candidate| candidate.piece));
                    }
                }
            }
        }
    }

    built.sort_by_key(|piece| piece.start);
    let painted_words = built
        .iter()
        .flat_map(|piece| piece.first_word..=piece.last_word)
        .collect::<HashSet<_>>()
        .len();
    TextFragmentPlan {
        directives: built.iter().map(|piece| piece.directive.clone()).collect(),
        source_word_intervals: built
            .iter()
            .map(|piece| TextFragmentWordInterval {
                quote_index: piece.quote_index,
                start: piece.start,
                end: piece.end,
                first_word: piece.first_word,
                last_word: piece.last_word,
            })
            .collect(),
        paint_quotes: built
            .iter()
            .map(|piece| {
                fragment_spelling_with_mode(
                    document.slice(piece.start, piece.end),
                    split_html_source_blocks,
                )
            })
            .collect(),
        source_safe_complete: complete,
        painted_words,
    }
}

pub fn text_fragment_plan(
    block_text: &str,
    document_text: Option<&str>,
    quotes: &[String],
    pdf: bool,
    publisher_may_annotate_legal_reference: bool,
    split_html_source_blocks: bool,
) -> TextFragmentPlan {
    let document = TextFragmentIndex(TextSearch::new(
        document_text
            .filter(|text| !js_trim(text).is_empty())
            .unwrap_or(block_text),
    ));
    text_fragment_plan_in(
        block_text,
        &document,
        quotes,
        pdf,
        publisher_may_annotate_legal_reference,
        split_html_source_blocks,
    )
}

/// A text's word index, built once for the plans of many quotations searched in it.
pub struct TextFragmentIndex<'a>(TextSearch<'a>);

pub fn text_fragment_index(text: &str) -> TextFragmentIndex<'_> {
    TextFragmentIndex(TextSearch::new(text))
}

/// The plan over a page's own text (`page`, the text a reader shows, as A2AJ Law Web's does): every seam prints as it
/// does there, so a term may hold one, and a printed label is context like any other word. One directive per block
/// where that paints the same words.
pub fn source_text_fragment_plan(block_text: &str, page: &TextFragmentPage, quotes: &[String]) -> TextFragmentPlan {
    let search = page.search();
    let source = FragmentText::Text(&search);
    let base = build_fragment_plan(block_text, source, quotes, false, false, false, true, false);
    let blocks = build_fragment_plan(block_text, source, quotes, false, false, true, true, false);
    if !blocks.source_safe_complete || crosses_multiple_lettered_breaks(source, &base) {
        base
    } else {
        blocks
    }
}

/// text_fragment_plan with its document text's index already built.
pub fn text_fragment_plan_in(
    block_text: &str,
    document: &TextFragmentIndex<'_>,
    quotes: &[String],
    pdf: bool,
    publisher_may_annotate_legal_reference: bool,
    split_html_source_blocks: bool,
) -> TextFragmentPlan {
    let source = FragmentText::Text(&document.0);
    let base = build_fragment_plan(
        block_text,
        source,
        quotes,
        pdf,
        publisher_may_annotate_legal_reference,
        false,
        false,
        false,
    );
    if !split_html_source_blocks {
        return base;
    }
    let blocks = build_fragment_plan(
        block_text,
        source,
        quotes,
        pdf,
        publisher_may_annotate_legal_reference,
        true,
        false,
        false,
    );
    if !blocks.source_safe_complete || crosses_multiple_lettered_breaks(source, &base) {
        base
    } else {
        blocks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for_publisher(
        block: &str,
        document: &str,
        quotes: &[&str],
        pdf: bool,
        publisher_may_annotate_legal_reference: bool,
    ) -> TextFragmentPlan {
        text_fragment_plan(
            block,
            Some(document),
            &quotes
                .iter()
                .map(|quote| (*quote).to_owned())
                .collect::<Vec<_>>(),
            pdf,
            publisher_may_annotate_legal_reference,
            false,
        )
    }

    fn plan(block: &str, document: &str, quotes: &[&str], pdf: bool) -> TextFragmentPlan {
        plan_for_publisher(block, document, quotes, pdf, false)
    }

    fn plan_for_html_blocks(block: &str, document: &str, quotes: &[&str]) -> TextFragmentPlan {
        text_fragment_plan(
            block,
            Some(document),
            &quotes
                .iter()
                .map(|quote| (*quote).to_owned())
                .collect::<Vec<_>>(),
            false,
            false,
            true,
        )
    }

    fn is_range_directive(directive: &str) -> bool {
        let mut pieces = directive
            .strip_prefix("text=")
            .unwrap_or(directive)
            .split(',')
            .collect::<Vec<_>>();
        if pieces.first().is_some_and(|piece| piece.ends_with('-')) {
            pieces.remove(0);
        }
        if pieces.last().is_some_and(|piece| piece.starts_with('-')) {
            pieces.pop();
        }
        pieces.len() == 2
    }

    fn pdf_full_exact_fallback(
        document: &str,
        first_word: usize,
        last_word: usize,
        context_first_word: usize,
        context_last_word: usize,
    ) -> Option<String> {
        let search = TextSearch::new(document);
        let fragment = FragmentText::Text(&search);
        let replay = BrowserReplay::new(fragment, false);
        let words = fragment.tokens();
        let mut planner = MaximalPlanner::new(&replay, true, false, false, false);
        planner.full_exact_directive_across_lines(MaximalCorePiece {
            start: words[first_word].start,
            end: words[last_word].end,
            first_word,
            last_word,
            context_first_word,
            context_last_word,
        })
    }

    #[test]
    fn maximal_plan_uses_one_range_and_exact_context_for_one_word() {
        let document = "Act means the first rule. Act means the second rule.";
        let one_word = plan("Act means the second rule.", document, &["Act"], false);
        assert!(one_word.source_safe_complete);
        assert_eq!(one_word.paint_quotes, ["Act"]);
        assert_eq!(one_word.painted_words, 1);
        assert_ne!(one_word.directives, ["text=Act"]);

        let unique_pdf = plan(
            "lead solitary tail",
            "lead solitary tail",
            &["solitary"],
            true,
        );
        assert!(unique_pdf.source_safe_complete);
        assert_eq!(unique_pdf.paint_quotes, ["solitary"]);
        assert_eq!(unique_pdf.directives, ["text=solitary"]);

        let markdown =
            "Regulations.\n\n**Act** means this statute. **senior officer** means this official.";
        let range = plan(
            markdown,
            markdown,
            &["Act means this statute senior officer means this official"],
            false,
        );
        assert!(range.source_safe_complete);
        assert_eq!(range.paint_quotes.len(), 1);
        assert_eq!(range.directives.len(), 1);
        assert!(is_range_directive(&range.directives[0]));
    }

    #[test]
    fn maximal_html_plan_ranges_across_plain_newlines_and_markdown_seams() {
        let html = "Unique lead Applicant and\nrespondent identify the issues to resolve tail";
        let exact = plan(
            html,
            html,
            &["Applicant and respondent identify the issues to"],
            false,
        );
        assert!(exact.source_safe_complete);
        assert_eq!(
            exact.paint_quotes,
            ["Applicant and respondent identify the issues to"]
        );
        assert_eq!(exact.directives.len(), 1);
        assert!(is_range_directive(&exact.directives[0]));

        let markdown =
            "Unique lead **Applicant and**\n**respondent identify the issues to** resolve tail";
        let split = plan(
            markdown,
            markdown,
            &["Applicant and respondent identify the issues to"],
            false,
        );
        assert!(split.source_safe_complete);
        assert_eq!(
            split.paint_quotes,
            ["Applicant and respondent identify the issues to"]
        );
        assert_eq!(split.directives.len(), 1);
        assert!(is_range_directive(&split.directives[0]));
    }

    #[test]
    fn html_block_plan_uses_one_range_across_source_seams() {
        let document =
            "Unique preface In this Act,\nactive member means a member who works. Unique tail";
        let quote = "In this Act, active member means a member who works";
        let plan = plan_for_html_blocks(document, document, &[quote]);

        assert!(plan.source_safe_complete, "plan={plan:?}");
        assert_eq!(plan.painted_words, quote.split_whitespace().count());
        assert_eq!(plan.directives.len(), 1, "plan={plan:?}");
        assert!(is_range_directive(&plan.directives[0]), "plan={plan:?}");
        assert_eq!(plan.paint_quotes, [quote]);
    }

    #[test]
    fn html_block_plan_splits_short_labelled_quote_across_three_blocks() {
        let document = concat!(
            "(2) In these regulations,\n",
            "\"acceptance inspection\" means an inspection of an\n",
            "elevating device that is conducted by an inspector."
        );
        let quote = concat!(
            "(2) In these regulations, \"acceptance inspection\" means an inspection of an ",
            "elevating device that is conducted"
        );
        let plan = plan_for_html_blocks(document, document, &[quote]);

        assert!(plan.source_safe_complete, "plan={plan:?}");
        assert_eq!(plan.directives.len(), 3, "plan={plan:?}");
        assert_eq!(
            plan.paint_quotes,
            [
                "In these regulations",
                "acceptance inspection\" means an inspection of an",
                "elevating device that is conducted",
            ]
        );
    }

    #[test]
    fn html_block_plan_omits_isolated_bilingual_definition_furniture() {
        let document = concat!(
            "Earlier definition. (« plan »)\n",
            "\"prescribed\" means prescribed by the regulations.\n",
            "\"property\" means the land."
        );
        let quote = concat!(
            "(« plan ») \"prescribed\" means prescribed by the regulations. ",
            "\"property\""
        );
        let plan = plan_for_html_blocks(document, document, &[quote]);

        assert!(plan.source_safe_complete, "plan={plan:?}");
        assert!(plan
            .paint_quotes
            .iter()
            .all(|paint| !paint.contains("plan")));
        assert!(plan
            .paint_quotes
            .iter()
            .any(|paint| paint.contains("prescribed")));
        assert!(plan
            .paint_quotes
            .iter()
            .any(|paint| paint.contains("property")));
    }

    #[test]
    fn html_sentence_seam_preserves_short_and_long_quotes() {
        let short = "prefix Okay. This is an oral decision in the claim suffix";
        let short_plan = plan(
            short,
            short,
            &["Okay. This is an oral decision in the claim"],
            false,
        );
        assert!(short_plan.source_safe_complete);
        assert_eq!(short_plan.painted_words, 9);
        assert_eq!(short_plan.directives.len(), 1);

        let quote = "child. It is about his best interests and not that of the parents. The court hopes that this decision will provide a framework for the parents to work together to parent the child. He deserves that from each of his parents and all of his extended family. It is apparent from the evidence that the child is loved by all of the family. The challenge will be in finding a";
        assert_eq!(quote.split_whitespace().count(), 70);
        let document = format!("about the {quote} way to proceed");
        let long_plan = plan(&document, &document, &[quote], false);
        assert!(long_plan.source_safe_complete);
        assert_eq!(long_plan.painted_words, 70);
        assert_eq!(long_plan.directives.len(), 1);
        assert!(is_range_directive(&long_plan.directives[0]));
    }

    #[test]
    fn maximal_html_plan_keeps_substantive_and_after_an_attached_paragraph_label() {
        let quote = "[5]AND CONSIDERING that the Rules are to be liberally interpreted with the aim of ensuring that";
        let plan = plan(quote, quote, &[quote], false);

        assert!(plan.source_safe_complete);
        assert_eq!(plan.painted_words, 16);
        assert_eq!(plan.paint_quotes.len(), 1);
        assert!(plan.paint_quotes[0].starts_with("AND CONSIDERING"));
        assert!(!plan.paint_quotes[0].contains("[5]"));
    }

    #[test]
    fn maximal_plan_keeps_substantive_bilingual_and_amendment_edges() {
        let leading = "d'accessibilité ») \"barrier\" means a barrier described in section 3";
        let leading_plan = plan(leading, leading, &[leading], false);
        assert!(leading_plan.source_safe_complete);
        assert_eq!(leading_plan.source_word_intervals[0].first_word, 0);
        assert!(leading_plan
            .paint_quotes
            .join(" ")
            .contains("d'accessibilité"));

        let trailing = "watering of livestock or poultry. (« fins domestiques »)";
        let trailing_plan = plan(trailing, trailing, &[trailing], false);
        assert!(trailing_plan.source_safe_complete);
        assert!(trailing_plan
            .paint_quotes
            .join(" ")
            .contains("fins domestiques"));

        let amendment = "by AR123/2024; (a) \"term\" means a value";
        let amendment_plan = plan(amendment, amendment, &[amendment], false);
        assert!(amendment_plan.source_safe_complete);
        assert!(amendment_plan.paint_quotes.join(" ").contains("AR123"));
    }

    #[test]
    fn maximal_html_plan_prefers_a_unique_exact_over_a_prefix_context_range() {
        let document = "Marker alpha beta gamma one tail. Other alpha beta gamma two tail.";
        let exact = plan(
            "Marker alpha beta gamma one tail.",
            document,
            &["alpha beta gamma one tail"],
            false,
        );

        assert!(exact.source_safe_complete);
        assert_eq!(exact.paint_quotes, ["alpha beta gamma one tail"]);
        assert_eq!(exact.directives.len(), 1);
        assert!(!is_range_directive(&exact.directives[0]));
        assert!(!exact.directives[0].contains("-,"));
    }

    #[test]
    fn maximal_html_plan_keeps_the_range_at_a_sentence_opening_end() {
        let document = "Marker alpha beta gamma one. Tail Other alpha beta gamma two. Tail";
        let range = plan(
            "Marker alpha beta gamma one. Tail",
            document,
            &["alpha beta gamma one. Tail"],
            false,
        );

        assert!(range.source_safe_complete);
        assert_eq!(range.paint_quotes, ["alpha beta gamma one. Tail"]);
        assert_eq!(range.directives.len(), 1);
        assert!(is_range_directive(&range.directives[0]));
    }

    #[test]
    fn maximal_pdf_full_exact_fallback_crosses_lines_only_when_unique() {
        let unique = pdf_full_exact_fallback("lead alpha\nbeta tail", 1, 2, 1, 2);
        assert_eq!(unique, Some("text=alpha%20beta".to_owned()));

        let repeated = pdf_full_exact_fallback("alpha\nbeta alpha\nbeta", 0, 1, 0, 1);
        assert_eq!(repeated, None);
    }

    #[test]
    fn maximal_pdf_full_exact_fallback_uses_distinct_unpainted_label_context() {
        let document =
            "(ii) the Agreement entered\ninto force\n(iii) the Agreement entered\ninto force";
        let directive = pdf_full_exact_fallback(document, 8, 11, 0, 11)
            .expect("the distinct label context selects the repeated full target");

        assert!(directive.contains("iii"));
        assert!(directive.contains("-,Agreement%20entered%20into%20force"));
    }

    #[test]
    fn maximal_plan_keeps_a_repeated_tail_when_ordered_replay_is_exact() {
        let document = "to the fund now\n20 th ancillary pension benefit—that is not a core pension benefit that";
        let range = plan(document, document, &[document], true);

        assert!(range.source_safe_complete);
        assert_eq!(range.painted_words, 17);
        let directive = range
            .directives
            .iter()
            .find(|directive| is_range_directive(directive))
            .expect("the split ordinal keeps range repair available");
        assert!(directive.ends_with(",benefit%20that"));
        assert!(range.paint_quotes.iter().any(|painted| painted
            == "to the fund now 20 th ancillary pension benefit—that is not a core pension benefit that"));
    }

    #[test]
    fn maximal_html_range_accepts_a_three_word_repeated_start_when_order_selects_desired_span() {
        let document = "alpha beta gamma desired omega. alpha beta gamma other omega.";
        let range = plan(
            "alpha beta gamma desired omega.",
            document,
            &["alpha beta gamma desired omega"],
            false,
        );

        assert!(range.source_safe_complete);
        assert_eq!(range.paint_quotes, ["alpha beta gamma desired omega"]);
        assert_eq!(range.directives, ["text=alpha%20beta%20gamma,omega."]);
        assert_eq!(range.source_word_intervals[0].first_word, 0);
        assert_eq!(range.source_word_intervals[0].last_word, 4);
    }

    #[test]
    fn maximal_html_range_grows_when_an_earlier_ordered_pair_would_be_wrong() {
        let document = "alpha wrong beta. marker alpha desired beta.";
        let range = plan(
            "marker alpha desired beta.",
            document,
            &["alpha desired beta"],
            false,
        );

        assert!(range.source_safe_complete);
        assert_eq!(range.paint_quotes, ["alpha desired beta"]);
        assert_eq!(range.directives.len(), 1);
        assert_ne!(range.directives[0], "text=alpha,beta");
        assert_eq!(range.source_word_intervals[0].first_word, 4);
        assert_eq!(range.source_word_intervals[0].last_word, 6);
    }

    #[test]
    fn maximal_plan_ranges_across_markdown_seams_with_endpoints_inside_pieces() {
        let document = "Unique marker **respect**\n**alpha including** one\nOther marker **respect**\n**beta including** two";
        let block = "Unique marker **respect**\n**alpha including** one";
        let split = plan(block, document, &["respect\nalpha including"], false);

        assert!(split.source_safe_complete);
        assert_eq!(split.paint_quotes, ["respect alpha including"]);
        assert_eq!(split.directives.len(), 1);
        assert!(is_range_directive(&split.directives[0]));
        assert!(split.directives[0].contains("respect"));
        assert!(split.directives[0].contains("including"));
    }

    #[test]
    fn maximal_pdf_plan_keeps_bare_unique_structural_islands() {
        let document = "yukon alpha unique\nbilingual beta distinct";
        let plan = plan(document, document, &[document], true);

        assert!(plan.source_safe_complete);
        assert_eq!(
            plan.paint_quotes,
            ["yukon alpha unique", "bilingual beta distinct"]
        );
        assert_eq!(plan.directives.len(), 2);
        assert!(plan
            .directives
            .iter()
            .all(|directive| !is_range_directive(directive)));
        assert_eq!(plan.painted_words, 6);
    }

    #[test]
    fn maximal_pdf_plan_repairs_only_a_context_dependent_middle_island() {
        let quote = "(1) left unique island\nrepeated middle words\nright unique island";
        let document = format!("{quote}\nunrelated repeated middle words elsewhere");
        let plan = plan(quote, &document, &[quote], true);

        assert!(plan.source_safe_complete);
        assert_eq!(plan.painted_words, 9);
        assert_eq!(plan.directives.len(), 4);
        assert_eq!(
            plan.directives
                .iter()
                .filter(|directive| is_range_directive(directive))
                .count(),
            1,
            "directives={:?} paint={:?}",
            plan.directives,
            plan.paint_quotes,
        );
        assert!(plan
            .paint_quotes
            .iter()
            .all(|painted| !painted.contains("(1)")));
        assert!(plan
            .source_word_intervals
            .iter()
            .any(|interval| interval.first_word == 1 && interval.last_word == 3));
        assert!(plan
            .source_word_intervals
            .iter()
            .any(|interval| interval.first_word == 7 && interval.last_word == 9));
        assert!(plan
            .source_word_intervals
            .iter()
            .any(|interval| interval.first_word == 4 && interval.last_word == 6));
        assert!(plan.source_word_intervals.iter().any(|interval| {
            interval.first_word <= 4
                && interval.last_word >= 6
                && interval.last_word - interval.first_word + 1 < 9
        }));
    }

    #[test]
    fn maximal_pdf_plan_retains_an_unrepairable_context_exact_island() {
        let one_word_document = "first marker Act ending\nsecond marker Act ending";
        let one_word = plan(
            "second marker Act ending",
            one_word_document,
            &["Act"],
            true,
        );
        assert!(one_word.source_safe_complete);
        assert_eq!(one_word.paint_quotes, ["Act"]);
        assert_eq!(one_word.painted_words, 1);
        assert_eq!(one_word.directives.len(), 1);
        assert!(!is_range_directive(&one_word.directives[0]));
        assert_ne!(one_word.directives, ["text=Act"]);
    }

    #[test]
    fn maximal_pdf_plan_locates_a_quote_split_by_source_lineation() {
        let text = "\"settlement area\" means, as the case may be,\n(a) the area described in appendix A to the\nGwich'in Agreement,\n(c) the area described in appendix A to the\nSahtu Agreement; (région désignée)\n\"Tłı̨chǫ Agreement\" means the Land Claims";
        let document = DocumentStructure::from_scalar_parts(
            "document".to_owned(),
            "fixture".to_owned(),
            text.to_owned(),
            "revision".to_owned(),
            None,
            crate::Scope::complete(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let block = "(c) the area described in appendix A to the Sahtu Agreement; ( région désignée ) \" Tłı̨chǫ Agreement\" means the Land Claims";
        let quote = "area described in appendix A to the Sahtu Agreement;";
        let plan = DocumentQuery::new().text_fragment_plan(
            &document,
            block,
            &[quote.to_owned()],
            true,
            false,
            false,
            false,
            false,
        );

        assert!(plan.source_safe_complete);
        assert_eq!(plan.painted_words, 9);
        assert!(!plan.directives.is_empty());
    }

    #[test]
    fn maximal_pdf_plan_uses_cross_line_context_without_painting_it() {
        let document = "(a) public body means a body under the Freedom of Information and Protection of Privacy Act;\n(z) public body means a public body as defined in clause 1(k) of the Freedom of\nInformation and Protection of Privacy Act;\n(aa) “Registrar” means the court registrar";
        let block = "(z) public body means a public body as defined in clause 1(k) of the Freedom of Information and Protection of Privacy Act; (aa) “Registrar” means the court registrar";
        let quote = "of Privacy Act; (aa) “Registrar” means";
        let plan = plan(block, document, &[quote], true);

        assert!(
            plan.source_safe_complete,
            "directives={:?} paint={:?}",
            plan.directives, plan.paint_quotes
        );
        assert_eq!(plan.painted_words, 5);
        assert!(plan
            .paint_quotes
            .iter()
            .any(|painted| painted == "of Privacy Act"));
        assert!(plan
            .paint_quotes
            .iter()
            .all(|painted| !painted.contains("Information and Protection")));
        assert!(plan
            .paint_quotes
            .iter()
            .all(|painted| !painted.contains("(aa)")));
    }

    #[test]
    fn maximal_pdf_plan_ranges_a_split_ordinal_token() {
        let quote = "left unique island\nDATED at Ottawa this 20 th day\nright unique island";
        let plan = plan(quote, quote, &[quote], true);

        assert!(plan.source_safe_complete);
        assert_eq!(plan.painted_words, 13);
        assert!(plan
            .directives
            .iter()
            .any(|directive| is_range_directive(directive)));
        assert!(!plan
            .paint_quotes
            .iter()
            .any(|painted| painted == "DATED at Ottawa this 20 th day"));
    }

    #[test]
    fn maximal_plan_splits_pdf_lines_and_only_trims_source_furniture() {
        let pdf = "In this Regulation “ class A society ”, in relation to a year means a society.";
        let opening = plan(
            pdf,
            pdf,
            &["Regulation “ class A society ”, in relation to"],
            true,
        );
        assert!(opening.source_safe_complete);
        assert_eq!(opening.painted_words, 7);
        assert_eq!(opening.source_word_intervals.len(), 1);
        assert_ne!(opening.directives[0], "text=Regulation");

        let furniture = "58.1 (3). Terms remain here.\n(4) For this purpose words remain.";
        let trimmed = plan(furniture, furniture, &[furniture], true);
        assert!(trimmed.source_safe_complete);
        assert_eq!(
            trimmed.paint_quotes,
            ["Terms remain here", "For this purpose words remain"]
        );
        assert_eq!(trimmed.painted_words, 8);

        let next_term = "bait means food « appât » “ standard";
        let complete = plan(next_term, next_term, &[next_term], true);
        assert!(complete.source_safe_complete);
        assert_eq!(complete.painted_words, 5);
        assert!(complete.paint_quotes.join(" ").contains("appât"));
        assert!(complete.paint_quotes.join(" ").ends_with("standard"));
        assert!(!complete.directives.is_empty());
        assert_eq!(
            complete
                .source_word_intervals
                .first()
                .zip(complete.source_word_intervals.last())
                .map(|(first, last)| (first.first_word, last.last_word)),
            Some((0, 4))
        );
    }

    #[test]
    fn maximal_plan_uses_compact_ranges_with_browser_boundary_replay() {
        let repeated = "unique pre alpha beta gamma delta. alpha other gamma delta.";
        let first = plan(
            "unique pre alpha beta gamma delta.",
            repeated,
            &["alpha beta gamma delta"],
            false,
        );
        assert!(first.source_safe_complete);
        assert_eq!(first.directives.len(), 1);
        assert_eq!(first.paint_quotes, ["alpha beta gamma delta"]);
        assert_eq!(first.painted_words, 4);
        assert_eq!(first.source_word_intervals[0].quote_index, 0);
        assert_eq!(
            first.source_word_intervals[0].end - first.source_word_intervals[0].start,
            "alpha beta gamma delta".len()
        );
        assert!(is_range_directive(&first.directives[0]));

        let punctuated = plan(
            "Unique lead equalization date applies.",
            "The prior equalization date\") was discarded. Unique lead equalization date applies.",
            &["equalization date"],
            false,
        );
        assert!(punctuated.source_safe_complete);
        assert_eq!(punctuated.source_word_intervals[0].first_word, 8);
        assert!(punctuated.directives[0].contains("-,"));

        let collated = plan(
            "unique the caesar finding",
            "the cæsar finding\nunique the caesar finding",
            &["the caesar finding"],
            false,
        );
        assert!(collated.source_safe_complete);
        assert!(collated.directives[0].contains("unique-"));

        let document = "Unique lead\nAlpha one\nBeta two\nTail end\nOther lead\nAlpha one\nBeta two\nOther end.";
        let block = "Unique lead\nAlpha one\nBeta two\nTail end";
        let html = plan(block, document, &["Alpha one\nBeta two"], false);
        assert!(html.source_safe_complete);
        assert_eq!(html.paint_quotes, ["Alpha one Beta two"]);
        assert_eq!(html.directives.len(), 1);
        assert!(is_range_directive(&html.directives[0]));

        let pdf = plan(block, document, &["Alpha one\nBeta two"], true);
        assert!(pdf.source_safe_complete);
        assert_eq!(pdf.painted_words, 4);
        assert!(pdf
            .paint_quotes
            .iter()
            .any(|painted| painted == "Alpha one"));
        assert!(pdf.paint_quotes.iter().any(|painted| painted == "Beta two"));
        assert!(pdf
            .paint_quotes
            .iter()
            .any(|painted| painted == "Alpha one Beta two"));
        assert!(!pdf.directives.is_empty());
        assert!(pdf
            .paint_quotes
            .iter()
            .all(|painted| { !painted.contains("Unique lead") && !painted.contains("Tail end") }));
        assert!(pdf
            .directives
            .iter()
            .any(|directive| is_range_directive(directive)));

        let legal_reference =
            "The duties arise under sections 3(1)(a). The minister acts promptly.";
        let reference = plan_for_publisher(
            legal_reference,
            legal_reference,
            &["duties arise under sections 3(1)(a) The minister acts promptly"],
            false,
            true,
        );
        assert!(reference.source_safe_complete);
        assert_eq!(reference.directives.len(), 1);
        assert!(is_range_directive(&reference.directives[0]));

        let ordinary_host = plan(
            legal_reference,
            legal_reference,
            &["duties arise under sections 3(1)(a) The minister acts promptly"],
            false,
        );
        assert_eq!(ordinary_host.directives.len(), 1);
        assert!(is_range_directive(&ordinary_host.directives[0]));

        for clustered in [
            "duties arise under sections 5.1.1 annotation follows",
            "duties arise under section 919.1(2)(a) annotation follows",
        ] {
            let clustered = plan_for_publisher(clustered, clustered, &[clustered], false, true);
            assert!(clustered.source_safe_complete);
            assert_eq!(clustered.directives.len(), 1);
            assert!(is_range_directive(&clustered.directives[0]));
        }

        let roles = "as the owner, the operator, or a director";
        let role_series = plan(roles, roles, &[roles], false);
        assert!(role_series.source_safe_complete);
        assert_eq!(role_series.painted_words, 8);
        assert!(!role_series.directives.is_empty());

        let multiple = plan(
            "Alpha first. Nearby beta second.",
            "Alpha first. Nearby beta second.",
            &["Alpha first", "beta second"],
            false,
        );
        assert!(multiple.source_safe_complete);
        assert_eq!(multiple.directives.len(), 2);
        assert_eq!(
            multiple
                .source_word_intervals
                .iter()
                .map(|interval| interval.quote_index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn maximal_plan_omits_only_classified_metadata_and_keeps_tight_atoms() {
        let signature = "at Ottawa Canada this 26th day of September 2011 Real Favreau Favreau J";
        let document = format!(
            "{signature}\nOLD RECORD\nexamined Signed\n{signature}\nCITATION 2011 TCC 418\nCOURT FILE Wayne Bowden Apostle"
        );
        let quote = format!("{signature} CITATION 2011 TCC 418 COURT FILE Wayne Bowden Apostle");
        let metadata = plan(
            &format!("examined Signed {quote}"),
            &document,
            &[&quote],
            true,
        );
        assert!(metadata.source_safe_complete);
        assert_eq!(metadata.painted_words, 8);
        assert!(metadata.paint_quotes.join(" ").starts_with("2011 TCC 418"));
        assert!(!metadata.paint_quotes.join(" ").contains("Ottawa"));

        let tight = "Protected Records:\n(a)information relating to prices";
        let atom = plan(
            tight,
            tight,
            &["Records: (a)information relating to prices"],
            false,
        );
        assert!(atom.source_safe_complete);
        assert_eq!(atom.painted_words, 6);
        assert!(atom.paint_quotes.join(" ").contains("information"));
    }
}
