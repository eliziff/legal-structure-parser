use super::search;
use super::*;
use crate::{javascript_whitespace, utf16_len};
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
    ) -> TextFragmentPlan {
        let text = self.text(document);
        build_fragment_plan(
            block_text,
            FragmentText::Document(self, document, &text),
            quotes,
            pdf,
            publisher_may_annotate_legal_reference,
        )
    }

    pub fn paragraph_range_directive(
        &self,
        document: &DocumentStructure,
        start: &str,
        end: &str,
    ) -> Option<String> {
        let text = self.text(document);
        let resolve = |locator: &str| {
            let label = self.requested_label(document, DocumentKind::Paragraph, locator);
            if label.is_empty() {
                return None;
            }
            self.unique_position(document, &label)
                .filter(|position| {
                    projected_kind(&document.nodes[position.node]) == Some(DocumentKind::Paragraph)
                })
                .map(|position| document.query_range(&document.nodes[position.node]))
        };
        let start = resolve(start)?;
        let end = resolve(end)?;
        if start.start > end.start {
            return None;
        }
        // This operation performs at least two phrase searches; build the index
        // on the first one instead of scanning the document first.
        self.searched.store(true, Ordering::Relaxed);
        let document = FragmentText::Document(self, document, &text);
        Some(
            match (
                unique_paragraph_edge(
                    js_trim(text.slice_utf16(start.start..start.end).unwrap_or_default()),
                    document,
                    true,
                ),
                unique_paragraph_edge(
                    js_trim(text.slice_utf16(end.start..end.end).unwrap_or_default()),
                    document,
                    false,
                ),
            ) {
                (Some(start), Some(end)) => text_range_directive(&start, &end),
                _ => String::new(),
            },
        )
    }
}

struct TextSearch<'a> {
    text: ScalarText<'a>,
    index: search::SearchIndex,
}

impl<'a> TextSearch<'a> {
    fn new(text: &'a str) -> Self {
        let scalar = ScalarText::new(text);
        Self {
            index: search::SearchIndex::with_scalar(text, &scalar),
            text: scalar,
        }
    }
}

#[derive(Clone, Copy)]
enum FragmentText<'a> {
    Document(&'a DocumentQuery, &'a DocumentStructure, &'a ScalarText<'a>),
    Text(&'a TextSearch<'a>),
}

impl<'a> FragmentText<'a> {
    fn tokens(self) -> &'a [search::WordOffset] {
        match self {
            Self::Document(query, document, text) => &query.index_with_text(document, text).tokens,
            Self::Text(query) => &query.index.tokens,
        }
    }

    fn phrase_spans(self, words: &[String], options: PhraseOptions) -> Vec<PhraseSpan> {
        match self {
            Self::Document(query, document, text) => {
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

    fn try_slice(self, start: usize, end: usize) -> Option<&'a str> {
        match self {
            Self::Document(_, _, text) => text.slice_utf16(start..end),
            Self::Text(query) => query.text.slice_utf16(start..end),
        }
    }

    fn utf16_len(self) -> usize {
        match self {
            Self::Document(_, _, text) => text.utf16_len(),
            Self::Text(query) => query.text.utf16_len(),
        }
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
        .map_or(0, |found| utf16_len(&value[..found.end()]))
}

fn extend_terminal_punctuation(text: FragmentText<'_>, end: usize, quote: &str) -> usize {
    let comma = js_trim(quote).ends_with(',');
    let mut extended = end;
    for character in text.slice(end, text.utf16_len()).chars() {
        if matches!(
            character,
            '.' | '!' | '?' | ';' | ':' | '…' | '\'' | '’' | '”' | '»' | ')' | ']'
        ) || comma && character == ','
        {
            extended += character.len_utf16();
        } else {
            break;
        }
    }
    extended
}

fn extend_leading_punctuation(text: FragmentText<'_>, start: usize, quote: &str) -> usize {
    let leading = quote
        .trim_start_matches(javascript_whitespace)
        .chars()
        .take_while(|character| matches!(character, '[' | '(' | '{' | '"' | '\'' | '‘' | '“' | '«'))
        .collect::<String>();
    let length = utf16_len(&leading);
    start
        .checked_sub(length)
        .filter(|from| !leading.is_empty() && text.slice(*from, start) == leading)
        .unwrap_or(start)
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
) -> PhraseSpan {
    static TRAILING_ARTIFACT: OnceLock<Regex> = OnceLock::new();
    static TRAILING_BILINGUAL: OnceLock<Regex> = OnceLock::new();
    static LEADING_ATTACHED_ORDER: OnceLock<Regex> = OnceLock::new();
    static LEADING_NUMBERED_ITEM: OnceLock<Regex> = OnceLock::new();
    static LEADING_AMENDMENT_LABEL: OnceLock<Regex> = OnceLock::new();
    static LEADING_BILINGUAL_TAIL: OnceLock<Regex> = OnceLock::new();
    static TRAILING_COMPLETE_BILINGUAL_LABEL: OnceLock<Regex> = OnceLock::new();
    static TRAILING_FRENCH_ACT_LABEL: OnceLock<Regex> = OnceLock::new();
    let mut start = original.start;
    let mut end = original.end;
    if trim_leading {
        let value = text.slice(start, end);
        let leading = [
            js_regex(
                r"(?u)^(\[\s*\d{1,4}\s*\]AND\s+)CONSIDERING\b",
                &LEADING_ATTACHED_ORDER,
            ),
            js_regex(r"(?u)^(\d{1,4}\s+[–—]\s+)\p{Lu}", &LEADING_NUMBERED_ITEM),
            js_regex(
                r#"(?iu)^(by\s+[\p{L}]{1,8}\d{1,6}/\d{1,4};\s*\([a-z]{1,3}\)\s*)[“\"][^”\"]+[”\"]\s+means\b"#,
                &LEADING_AMENDMENT_LABEL,
            ),
            js_regex(
                r#"(?iu)^([\p{L}][\p{L}'’.-]*(?:\s+[\p{L}][\p{L}'’.-]*){0,3}\s*(?:(?:»|Â»)\s*)?\)\s*)[“\"][^”\"\r\n]{1,80}[”\"]\s+means\b"#,
                &LEADING_BILINGUAL_TAIL,
            ),
        ]
        .into_iter()
        .find_map(|pattern| pattern.captures(value))
        .and_then(|captures| captures.get(1));
        if let Some(leading) = leading {
            start += utf16_len(leading.as_str());
        }
        loop {
            let length = leading_label_length(text.slice(start, end));
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
    if let Some(bilingual) =
        js_regex(r"(?u)\s*\(\s*(?:«|Â«)\s*[^)]*$", &TRAILING_BILINGUAL).find(text.slice(start, end))
    {
        if bilingual.start() > 0 {
            end = start + utf16_len(&text.slice(start, end)[..bilingual.start()]);
        }
    }
    for pattern in [
        js_regex(
            r"(?u)\s*;\s*(?:«|Â«)\s*[^»\r\n]{1,80}(?:»|Â»)\s*$",
            &TRAILING_COMPLETE_BILINGUAL_LABEL,
        ),
        js_regex(
            r"(?iu)\s*;\s*\(\s*(?:loi|règlement)\s*$",
            &TRAILING_FRENCH_ACT_LABEL,
        ),
    ] {
        if let Some(edge) = pattern.find(text.slice(start, end)) {
            if edge.start() > 0 {
                end = start + utf16_len(&text.slice(start, end)[..edge.start()]);
            }
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
    let extend = |span: &mut PhraseSpan| {
        span.start = extend_leading_punctuation(text, span.start, quote);
        span.end = extend_terminal_punctuation(text, span.end, quote);
    };
    let mut spans = text.phrase_spans(words, PhraseOptions::default());
    for span in &mut spans {
        extend(span);
    }
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
        for span in &mut spans {
            extend(span);
        }
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

fn text_directive(target: &str, prefix: &str, suffix: &str) -> String {
    let target = encoded_fragment(target);
    let prefix = encoded_context(prefix, true);
    let suffix = encoded_context(suffix, false);
    format!("text={prefix}{target}{suffix}")
}

fn text_range_directive(start: &str, end: &str) -> String {
    text_range_directive_with_context(start, end, "", "")
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

fn directive_match_count(
    document: FragmentText<'_>,
    target: &str,
    prefix: &str,
    suffix: &str,
) -> usize {
    document
        .phrase_spans(
            &[prefix, target, suffix]
                .into_iter()
                .flat_map(quote_words)
                .collect::<Vec<_>>(),
            PhraseOptions {
                limit: Some(2),
                ..Default::default()
            },
        )
        .len()
}

fn browser_key(value: &str) -> String {
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
    key: String,
    words: Vec<String>,
    leading_punctuation: bool,
    trailing_punctuation: bool,
}

fn browser_spelled_term(value: &str) -> Option<BrowserSpelledTerm> {
    let text = fragment_spelling(value);
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
        leading_punctuation,
        trailing_punctuation: last_end < text.len(),
        key: browser_key(&text),
        words,
    })
}

#[derive(Default)]
struct ExactDirectiveReplay {
    first: Option<PhraseSpan>,
    count: usize,
}

struct BrowserReplay<'a> {
    document: FragmentText<'a>,
    words: Vec<String>,
    postings: HashMap<u64, Vec<usize>>,
}

impl<'a> BrowserReplay<'a> {
    fn new(document: FragmentText<'a>) -> Self {
        let tokens = document.tokens();
        let mut postings = HashMap::<u64, Vec<usize>>::new();
        let mut words = Vec::with_capacity(tokens.len());
        for (index, token) in tokens.iter().enumerate() {
            let literal = document.slice(token.start, token.end).to_lowercase();
            let word = browser_key(&literal);
            postings
                .entry(search::word_hash(&word))
                .or_default()
                .push(index);
            words.push(word);
        }
        Self {
            document,
            words,
            postings,
        }
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
        if wanted.is_empty() || first_word + wanted.len() > self.words.len() {
            return None;
        }
        if self.words[first_word..first_word + wanted.len()] != *wanted {
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
                let rendered = fragment_spelling(self.document.slice(start, end));
                if browser_key(&rendered).eq(&term.key) {
                    return Some(matched);
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
        let prefix_term = (!prefix.is_empty()).then(|| browser_spelled_term(prefix));
        let suffix_term = (!suffix.is_empty()).then(|| browser_spelled_term(suffix));
        if prefix_term.as_ref().is_some_and(Option::is_none)
            || suffix_term.as_ref().is_some_and(Option::is_none)
        {
            return ExactDirectiveReplay::default();
        }
        let prefix_term = prefix_term.flatten();
        let suffix_term = suffix_term.flatten();
        let first_term = prefix_term.as_ref().unwrap_or(target_term);
        let Some(candidates) = self.postings.get(&search::word_hash(&first_term.words[0])) else {
            return ExactDirectiveReplay::default();
        };
        let mut replay = ExactDirectiveReplay::default();
        for &first_word in candidates {
            let prefix_match = prefix_term
                .as_ref()
                .and_then(|prefix| self.spelled_term_at(prefix, first_word, allow_across_lines));
            if prefix_term.is_some() && prefix_match.is_none() {
                continue;
            }
            let target_first = prefix_match.map_or(first_word, |matched| matched.last_word + 1);
            let Some(target_match) =
                self.spelled_term_at(target_term, target_first, allow_across_lines)
            else {
                continue;
            };
            if suffix_term.as_ref().is_some_and(|suffix| {
                self.spelled_term_at(suffix, target_match.last_word + 1, allow_across_lines)
                    .is_none()
            }) {
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
}

fn fragment_spelling(value: &str) -> String {
    static MARKDOWN: OnceLock<Regex> = OnceLock::new();
    static OPENING_QUOTE_SPACE: OnceLock<Regex> = OnceLock::new();
    static OPENING_BRACKET_SPACE: OnceLock<Regex> = OnceLock::new();
    static CLOSING_SPACE: OnceLock<Regex> = OnceLock::new();
    let normalized = normalize_blank_whitespace(value);
    let unmarked = js_regex(r"[*_`]", &MARKDOWN)
        .replace_all(&normalized, "")
        .into_owned();
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
    let marker_length = leading_label_length(&line[indent..]);
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

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFragmentWordInterval {
    pub quote_index: usize,
    pub start: usize,
    pub end: usize,
    pub first_word: usize,
    pub last_word: usize,
}

#[derive(Clone, Serialize)]
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
    atom_cache: HashMap<usize, LexicalTerm>,
    atomic_options_cache: HashMap<usize, Vec<LexicalTerm>>,
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
    ) -> Self {
        Self {
            replay,
            pdf,
            publisher_may_annotate_legal_reference,
            atom_cache: HashMap::new(),
            atomic_options_cache: HashMap::new(),
            directive_cache: HashMap::new(),
            furniture_cache: HashMap::new(),
        }
    }

    fn line_first_word(&self, word: usize) -> usize {
        source_line_first_word(self.replay, word)
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
            text: fragment_spelling(self.replay.document.slice(start, end)),
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
            text: fragment_spelling(
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

    fn safe_lexical_gap(&self, left: &LexicalTerm, right: &LexicalTerm) -> bool {
        let gap = self.replay.document.slice(left.end, right.start);
        !gap.chars()
            .any(|character| matches!(character, '\r' | '\n'))
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
        if gap
            .chars()
            .any(|character| matches!(character, '\r' | '\n'))
            || self.pdf && js_regex(r#"(?u)(?:»|Â»)\s*[“\"]"#, &BILINGUAL_RECORD).is_match(gap)
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

    fn exact_target_for(&mut self, piece: MaximalCorePiece) -> Option<LexicalTerm> {
        for word in piece.first_word..piece.last_word {
            if self.has_structural_seam(word, word + 1) {
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
            text: fragment_spelling(self.replay.document.slice(start, end)),
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
        browser_spelled_term(target).map_or_else(ExactDirectiveReplay::default, |term| {
            self.replay.replay_exact(&term, prefix, suffix, false)
        })
    }

    fn endpoint_terms_from_start(&mut self, piece: MaximalCorePiece) -> Vec<LexicalTerm> {
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
            if next.last_word > piece.last_word || !self.safe_lexical_gap(&last, &next) {
                break;
            }
            last = next;
            terms.push(LexicalTerm {
                text: fragment_spelling(self.replay.document.slice(first_atom.start, last.end)),
                start: first_atom.start,
                end: last.end,
                first_word: piece.first_word,
                last_word: last.last_word,
                form: LexicalForm::Run,
            });
        }
        terms
    }

    fn endpoint_terms_from_end(&mut self, piece: MaximalCorePiece) -> Vec<LexicalTerm> {
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
            if previous.first_word < piece.first_word || !self.safe_lexical_gap(&previous, &first) {
                break;
            }
            first = previous;
            terms.push(LexicalTerm {
                text: fragment_spelling(self.replay.document.slice(first.start, last_atom.end)),
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
        let marker_length = leading_label_length(self.replay.document.slice(span.start, span.end));
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
        if self.atom_at(first_word)?.first_word < first_word {
            return Some(span);
        }
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
        let adjacent_first = if same_line_first > 0 {
            self.line_first_word(same_line_first - 1)
        } else {
            same_line_first
        };
        let adjacent_last = if same_line_last + 1 < words.len() {
            self.line_last_word(same_line_last + 1)
        } else {
            same_line_last
        };
        MaximalCorePiece {
            start: span.start,
            end: span.end,
            first_word: span.first_word,
            last_word: span.last_word,
            context_first_word: if clamp_leading_context {
                span.first_word
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
    ) -> Vec<MaximalCorePiece> {
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
            if let Some(trimmed) = planner.trim_leading_furniture(raw) {
                pieces.push(planner.core_piece(
                    trimmed,
                    clamp_leading_context && first_word == desired.first_word
                        || trimmed.first_word != raw.first_word,
                    clamp_trailing_context && last_word == desired.last_word,
                ));
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
        let marker_last = line_start_furniture_last_word(self.replay.document, first, last);
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

    fn contains_label_furniture(&mut self, term: &LexicalTerm) -> bool {
        (term.first_word..=term.last_word)
            .any(|word| self.source_furniture(word) == Some(FurnitureKind::Label))
    }

    fn context_allowed(
        &mut self,
        term: &LexicalTerm,
        desired: PhraseSpan,
        required: &HashSet<usize>,
    ) -> bool {
        for word in term.first_word..=term.last_word {
            let furniture = self.source_furniture(word);
            if word >= desired.first_word && word <= desired.last_word {
                if !required.contains(&word)
                    && (furniture != Some(FurnitureKind::Label) || self.pdf)
                {
                    return false;
                }
            } else if furniture == Some(FurnitureKind::Metadata)
                || furniture == Some(FurnitureKind::Label) && self.pdf
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
            if !self.context_allowed(&term, desired, required) {
                continue;
            }
            let unique = self.replay_exact_text(&term.text, "", "").count == 1;
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
        if before.is_some_and(|term| self.contains_label_furniture(term))
            || after.is_some_and(|term| self.contains_label_furniture(term))
        {
            return 2;
        }
        if before.is_some_and(|term| self.has_structural_seam(term.last_word, piece.first_word))
            || after.is_some_and(|term| self.has_structural_seam(piece.last_word, term.first_word))
        {
            return 4;
        }
        if before.is_some_and(|term| {
            term.first_word < requested_first || term.last_word > requested_last
        }) || after.is_some_and(|term| {
            term.first_word < requested_first || term.last_word > requested_last
        }) {
            3
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
        for target in exact_targets {
            for (before, after) in contexts {
                if target.form == LexicalForm::Word
                    && before.is_none()
                    && after.is_none()
                    && !(self.pdf && self.replay_exact_text(&target.text, "", "").count == 1)
                {
                    continue;
                }
                let selected = self
                    .replay_exact_text(
                        &target.text,
                        before.as_ref().map_or("", |term| term.text.as_str()),
                        after.as_ref().map_or("", |term| term.text.as_str()),
                    )
                    .first;
                if !selects(piece, selected) {
                    continue;
                }
                consider(
                    &mut shortest,
                    text_directive(
                        &target.text,
                        before.as_ref().map_or("", |term| term.text.as_str()),
                        after.as_ref().map_or("", |term| term.text.as_str()),
                    ),
                );
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

        let bare = vec![(None, None)];
        let mut directive = self.shortest_for(piece, &exact_targets, &bare);
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
                let terms = self.endpoint_terms_from_end(context);
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
                let terms = self.endpoint_terms_from_start(context);
                self.shortest_sufficient_context(terms, desired, required)
            } else {
                Vec::new()
            };
            let mut contexts_by_class =
                [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
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
            for contexts in contexts_by_class.iter().skip(1) {
                directive = self.shortest_for(piece, &exact_targets, contexts);
                if directive.is_some() {
                    break;
                }
            }
        }
        self.directive_cache.insert(key, directive.clone());
        directive
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

fn build_fragment_plan(
    block_text: &str,
    document: FragmentText<'_>,
    quotes: &[String],
    pdf: bool,
    publisher_may_annotate_legal_reference: bool,
) -> TextFragmentPlan {
    let block_search = TextSearch::new(block_text);
    let block = FragmentText::Text(&block_search);
    let replay = BrowserReplay::new(document);
    let mut planner = MaximalPlanner::new(&replay, pdf, publisher_may_annotate_legal_reference);
    let source_words = document.tokens();
    let mut built = Vec::<BuiltPiece>::new();
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
        let adjusted = adjust_span_edges_with_options(block, selected, true);
        let Some(desired) = locate_document_quote(block, document, adjusted) else {
            complete = false;
            continue;
        };
        let clamp_leading = adjusted.start > selected.start;
        let clamp_trailing = adjusted.end < selected.end;
        let line_pieces = planner.source_pieces(desired, clamp_leading, clamp_trailing);
        let mut required = line_pieces
            .iter()
            .flat_map(|piece| piece.first_word..=piece.last_word)
            .collect::<HashSet<_>>();
        for word in duplicate_signature_metadata(document, desired) {
            required.remove(&word);
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
            let whole_stays_in_structural_piece = line_pieces
                .iter()
                .any(|piece| piece.first_word <= first_word && piece.last_word >= last_word);
            if whole_stays_in_structural_piece {
                if let Some(directive) = planner.atomic_directive_for(
                    whole,
                    quote_index,
                    first_word,
                    last_word,
                    desired,
                    &required,
                ) {
                    built.push(planner.candidate_for(whole, quote_index, directive).piece);
                    continue;
                }
            }

            let mut candidates = Vec::<Candidate>::new();
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
                let piece = planner.core_piece(
                    PhraseSpan {
                        start: source_words[piece_first].start,
                        end: source_words[piece_last].end,
                        first_word: piece_first,
                        last_word: piece_last,
                    },
                    clamp_leading && piece_first == desired.first_word,
                    clamp_trailing && piece_last == desired.last_word,
                );
                if let Some(candidate) = planner.candidate_for_piece(
                    piece,
                    quote_index,
                    first_word,
                    last_word,
                    desired,
                    &required,
                ) {
                    add_candidate(candidate, &mut candidates, &mut candidate_keys);
                }
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
            .map(|piece| fragment_spelling(document.slice(piece.start, piece.end)))
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
) -> TextFragmentPlan {
    let document = TextSearch::new(
        document_text
            .filter(|text| !js_trim(text).is_empty())
            .unwrap_or(block_text),
    );
    build_fragment_plan(
        block_text,
        FragmentText::Text(&document),
        quotes,
        pdf,
        publisher_may_annotate_legal_reference,
    )
}

fn unique_paragraph_edge(
    text: &str,
    document: FragmentText<'_>,
    from_start: bool,
) -> Option<String> {
    static LEADING_PARAGRAPH_LABEL: OnceLock<Regex> = OnceLock::new();
    let line = text
        .split('\n')
        .next()
        .unwrap_or_default()
        .trim_end_matches('\r');
    let block = js_regex(
        r"(?u)^\s*(?:\[[0-9]+\]|[0-9]+[.)])\s*",
        &LEADING_PARAGRAPH_LABEL,
    )
    .replace(line, "")
    .into_owned();
    let scalar = ScalarText::new(&block);
    let words = search::tokenize_with_scalar(&block, &scalar);
    for length in [12, 16, 8, 24, 32, 6, 4, 2] {
        if words.len() < length {
            continue;
        }
        let edge = if from_start {
            &words[..length]
        } else {
            &words[words.len() - length..]
        };
        let target = normalize_blank_whitespace(
            scalar
                .slice_utf16(edge[0].start..edge.last()?.end)
                .unwrap_or_default(),
        );
        if directive_match_count(document, &target, "", "") == 1 {
            return Some(target);
        }
    }
    None
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
        )
    }

    fn plan(block: &str, document: &str, quotes: &[&str], pdf: bool) -> TextFragmentPlan {
        plan_for_publisher(block, document, quotes, pdf, false)
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

    #[test]
    fn maximal_plan_uses_exact_html_pieces_and_context_for_one_word() {
        let document = "Act means the first rule. Act means the second rule.";
        let one_word = plan("Act means the second rule.", document, &["Act"], false);
        assert!(one_word.source_safe_complete);
        assert_eq!(one_word.paint_quotes, ["Act"]);
        assert_eq!(one_word.painted_words, 1);
        assert_ne!(one_word.directives, ["text=Act"]);

        let markdown =
            "Regulations.\n\n**Act** means this statute. **senior officer** means this official.";
        let exact_cover = plan(
            markdown,
            markdown,
            &["Act means this statute senior officer means this official"],
            false,
        );
        assert!(exact_cover.source_safe_complete);
        assert_eq!(
            exact_cover.paint_quotes,
            [
                "Act means this statute",
                "senior officer means this official"
            ]
        );
        assert_eq!(exact_cover.directives.len(), 2);
        assert!(exact_cover
            .directives
            .iter()
            .all(|item| !is_range_directive(item)));
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
        assert!(complete
            .directives
            .iter()
            .all(|item| !is_range_directive(item)));
        assert_eq!(
            complete
                .source_word_intervals
                .last()
                .map(|interval| (interval.first_word, interval.last_word)),
            Some((4, 4))
        );
    }

    #[test]
    fn maximal_plan_uses_exact_cover_with_browser_boundary_replay() {
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
        assert!(first
            .directives
            .iter()
            .all(|item| !is_range_directive(item)));

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
        assert_eq!(html.paint_quotes, ["Alpha one", "Beta two"]);
        assert_eq!(html.directives.len(), 2);
        assert!(html.directives.iter().all(|item| !is_range_directive(item)));

        let pdf = plan(block, document, &["Alpha one\nBeta two"], true);
        assert!(pdf.source_safe_complete);
        assert_eq!(pdf.painted_words, 4);
        assert_eq!(pdf.paint_quotes, ["Alpha one", "Beta two"]);
        assert_eq!(pdf.directives.len(), 2);
        assert!(pdf.directives.iter().all(|item| !is_range_directive(item)));

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
        assert_eq!(reference.directives.len(), 2);
        assert!(reference
            .directives
            .iter()
            .all(|item| !is_range_directive(item)));

        let ordinary_host = plan(
            legal_reference,
            legal_reference,
            &["duties arise under sections 3(1)(a) The minister acts promptly"],
            false,
        );
        assert_eq!(ordinary_host.directives.len(), 1);

        for clustered in [
            "duties arise under sections 5.1.1 annotation follows",
            "duties arise under section 919.1(2)(a) annotation follows",
        ] {
            let clustered = plan_for_publisher(clustered, clustered, &[clustered], false, true);
            assert!(clustered.source_safe_complete);
            assert_eq!(clustered.directives.len(), 2);
            assert!(clustered
                .directives
                .iter()
                .all(|item| !is_range_directive(item)));
        }

        let roles = "as the owner, the operator, or a director";
        let role_series = plan(roles, roles, &[roles], false);
        assert!(role_series.source_safe_complete);
        assert_eq!(role_series.painted_words, 8);
        assert_eq!(role_series.directives.len(), 4);
        assert!(role_series
            .directives
            .iter()
            .all(|item| !is_range_directive(item)));
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
        assert!(atom.paint_quotes.join(" ").contains("(a)information"));
    }
}
