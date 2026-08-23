use super::*;

impl DocumentQuery {
    pub fn text_fragment_directives(
        &self,
        document: &DocumentStructure,
        block_text: &str,
        quotes: &[String],
        page_scoped: bool,
    ) -> Vec<String> {
        let text = ScalarText::new(document.query_text());
        fragment_directives(
            block_text,
            FragmentText::Document(self, document, &text),
            quotes,
            page_scoped,
        )
    }

    pub fn paragraph_range_directive(
        &self,
        document: &DocumentStructure,
        start: &str,
        end: &str,
    ) -> Option<String> {
        let text = ScalarText::new(document.query_text());
        let start = self
            .lookup_with_text(document, DocumentKind::Paragraph, start, 0, Some(&text))
            .block?;
        let end = self
            .lookup_with_text(document, DocumentKind::Paragraph, end, 0, Some(&text))
            .block?;
        if start.block.start > end.block.start {
            return None;
        }
        let document = FragmentText::Document(self, document, &text);
        Some(
            match (
                unique_paragraph_edge(&start.text, document, true),
                unique_paragraph_edge(&end.text, document, false),
            ) {
                (Some(start), Some(end)) => text_range_directive(&start, &end),
                _ => String::new(),
            },
        )
    }
}

struct TextSearch<'a> {
    text: ScalarText<'a>,
    index: SearchIndex,
    line_breaks: Vec<usize>,
}

impl<'a> TextSearch<'a> {
    fn new(text: &'a str) -> Self {
        let scalar = ScalarText::new(text);
        let line_breaks = text
            .match_indices('\n')
            .map(|(byte, _)| scalar.utf16_at_byte(byte).expect("line break boundary"))
            .collect();
        Self {
            index: SearchIndex::with_scalar(text, &scalar),
            text: scalar,
            line_breaks,
        }
    }
}

#[derive(Clone, Copy)]
enum FragmentText<'a> {
    Document(&'a DocumentQuery, &'a DocumentStructure, &'a ScalarText<'a>),
    Text(&'a TextSearch<'a>),
}

impl<'a> FragmentText<'a> {
    fn tokens(self) -> &'a [WordOffset] {
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
            Self::Text(query) => indexed_phrase_spans(
                &query.index,
                &query.line_breaks,
                &query.text,
                words,
                options,
            ),
        }
    }

    fn slice(self, start: usize, end: usize) -> &'a str {
        match self {
            Self::Document(_, _, text) => text.slice_utf16(start..end).unwrap_or_default(),
            Self::Text(query) => query.text.slice_utf16(start..end).unwrap_or_default(),
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
    static BARE_NUMBER: OnceLock<Regex> = OnceLock::new();
    let trimmed_end = value.trim_end_matches(javascript_whitespace);
    let label = LABELS
        .get_or_init(|| {
            [
                r"(?u)^\[\s*[0-9]{1,4}\s*\]\s*",
                r"(?u)^[0-9]{1,4}\]\s*",
                r"(?u)^[0-9]{1,4}(?:\.[0-9]{1,4})*\s*(?:\(\s*[A-Za-z0-9]{1,5}\s*\)\s*)+",
                r"(?u)^\(\s*[A-Za-z0-9]{1,5}\s*\)\s*",
                r"(?u)^[A-Za-z]{1,3}\)\s*",
                r"(?u)^[0-9]{1,4}[.)]\s*",
            ]
            .map(|pattern| Regex::new(&pattern.replace(r"\s", JS_WS)).expect("literal label regex"))
            .into()
        })
        .iter()
        .filter_map(|pattern| pattern.find(value))
        .find(|found| found.end() < trimmed_end.len());
    let label = label.or_else(|| {
        let found = js_regex(r"(?u)^[0-9]{1,4}\s+", &BARE_NUMBER).find(value)?;
        matches!(
            value[found.end()..].chars().next(),
            Some('A'..='Z' | '“' | '"' | '(')
        )
        .then_some(found)
    });
    label.map_or(0, |found| utf16_len(&value[..found.end()]))
}

fn strip_leading_labels(value: &str) -> String {
    let scalar = ScalarText::new(value);
    let mut start = 0;
    loop {
        let length = leading_label_length(
            scalar
                .slice_utf16(start..scalar.utf16_len())
                .unwrap_or_default(),
        );
        if length == 0 {
            return js_trim(
                scalar
                    .slice_utf16(start..scalar.utf16_len())
                    .unwrap_or_default(),
            )
            .to_owned();
        }
        start += length;
    }
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

fn adjust_span_edges(text: FragmentText<'_>, original: PhraseSpan) -> PhraseSpan {
    static TRAILING_ARTIFACT: OnceLock<Regex> = OnceLock::new();
    let mut start = original.start;
    let mut end = original.end;
    loop {
        let length = leading_label_length(text.slice(start, end));
        if length == 0 {
            break;
        }
        start += length;
    }
    loop {
        let Some(artifact) = js_regex(
            r"(?u)\s*[.,;:]?\[\s*[0-9]{1,4}(?:\s*[-–—,;]\s*[0-9]{1,4})*\s*\]\s*$",
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

fn edge_phrase(
    text: FragmentText<'_>,
    span: &PhraseSpan,
    from_start: bool,
    size: usize,
) -> Option<(String, usize, usize)> {
    let words = text.tokens();
    let mut indexes = Vec::<usize>::with_capacity(size);
    let mut index = if from_start {
        span.first_word as isize
    } else {
        span.last_word as isize
    };
    while indexes.len() < size
        && if from_start {
            index <= span.last_word as isize
        } else {
            index >= span.first_word as isize
        }
    {
        let current = index as usize;
        let token = words.get(current)?;
        if let Some(&previous) = indexes.last() {
            let previous = &words[previous];
            if text
                .slice(previous.end.min(token.start), previous.end.max(token.start))
                .contains('\n')
            {
                break;
            }
        }
        indexes.push(current);
        index += if from_start { 1 } else { -1 };
    }
    let first = *indexes.iter().min()?;
    let last = *indexes.iter().max()?;
    let raw = normalize_blank_whitespace(text.slice(words[first].start, words[last].end));
    let value = strip_leading_labels(&raw);
    (!quote_words(&value).is_empty()).then_some((value, first, last))
}

fn range_directive_match_count(document: FragmentText<'_>, start: &str, end: &str) -> usize {
    let starts = document.phrase_spans(
        &quote_words(start),
        PhraseOptions {
            limit: Some(3),
            ..Default::default()
        },
    );
    if starts.is_empty() {
        return 0;
    }
    let ends = document.phrase_spans(
        &quote_words(end),
        PhraseOptions {
            limit: Some(3),
            ..Default::default()
        },
    );
    starts
        .iter()
        .filter(|start| ends.iter().any(|end| end.start >= start.end))
        .take(2)
        .count()
}

fn build_range_directive(
    block: FragmentText<'_>,
    span: &PhraseSpan,
    document: FragmentText<'_>,
) -> Option<(Vec<String>, usize)> {
    for size in [6, 4, 8, 12] {
        let (Some((head, _, head_last)), Some((tail, tail_first, _))) = (
            edge_phrase(block, span, true, size),
            edge_phrase(block, span, false, size),
        ) else {
            continue;
        };
        if head_last >= tail_first || range_directive_match_count(document, &head, &tail) != 1 {
            continue;
        }
        let padded_head = citation_cluster_variant(&head);
        let padded_tail = citation_cluster_variant(&tail);
        let mut directives = vec![text_range_directive(&head, &tail)];
        if padded_head.is_some() || padded_tail.is_some() {
            directives.push(text_range_directive(
                padded_head.as_deref().unwrap_or(&head),
                padded_tail.as_deref().unwrap_or(&tail),
            ));
        }
        return Some((directives, span.start));
    }
    None
}

fn choose_source_span(text: FragmentText<'_>, quote: &str) -> Option<PhraseSpan> {
    let mut spans = text.phrase_spans(&quote_words(quote), PhraseOptions::default());
    for span in &mut spans {
        span.end = extend_terminal_punctuation(text, span.end, quote);
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
    let wanted = [
        normalize_blank_whitespace(
            js_trim(quote).trim_matches(|character| matches!(character, '"' | '\'' | '“' | '”')),
        ),
        quote_text(quote),
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
    None
}

fn encode_text_fragment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'~') {
            encoded.push(*byte as char);
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

fn text_directive(target: &str, prefix: &str, suffix: &str) -> String {
    let target = encode_text_fragment(&normalize_blank_whitespace(target));
    let prefix = (!prefix.is_empty())
        .then(|| {
            format!(
                "{}-,",
                encode_text_fragment(&normalize_blank_whitespace(prefix))
            )
        })
        .unwrap_or_default();
    let suffix = (!suffix.is_empty())
        .then(|| {
            format!(
                ",-{}",
                encode_text_fragment(&normalize_blank_whitespace(suffix))
            )
        })
        .unwrap_or_default();
    format!("text={prefix}{target}{suffix}")
}

fn text_range_directive(start: &str, end: &str) -> String {
    format!(
        "text={},{}",
        encode_text_fragment(&normalize_blank_whitespace(start)),
        encode_text_fragment(&normalize_blank_whitespace(end))
    )
}

fn citation_cluster_variant(target: &str) -> Option<String> {
    static CLUSTER: OnceLock<Regex> = OnceLock::new();
    static FIRST: OnceLock<Regex> = OnceLock::new();
    static SECOND: OnceLock<Regex> = OnceLock::new();
    if !js_regex(r"(?u)[A-Za-z]{1,3}\.\s[0-9]", &CLUSTER).is_match(target) {
        return None;
    }
    let padded = js_regex(r"(?-u:\b)([A-Za-z]{1,3}\.)(?:\s)([0-9])", &FIRST)
        .replace_all(target, "$1\u{00a0}$2");
    let padded = js_regex(
        r"(?u)(?-u:\b)([A-Za-z]{1,3}\.\u{00a0}[0-9]+[A-Za-z0-9.]*)(?:\s)",
        &SECOND,
    )
    .replace_all(&padded, "$1\u{00a0} ")
    .into_owned();
    (padded != target).then_some(padded)
}

fn exact_directives(target: &str, prefix: &str, suffix: &str) -> Vec<String> {
    let mut directives = vec![text_directive(target, prefix, suffix)];
    if let Some(variant) = citation_cluster_variant(target) {
        directives.push(text_directive(&variant, prefix, suffix));
    }
    directives
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

fn context_for(block: FragmentText<'_>, span: &PhraseSpan, window: usize) -> (String, String) {
    static LEADING_CONTEXT_LABEL: OnceLock<Regex> = OnceLock::new();
    let words = block.tokens();
    let first_prefix_word = span.first_word.saturating_sub(window);
    let last_suffix_word = (span.last_word + window).min(words.len().saturating_sub(1));
    let prefix = (span.first_word > first_prefix_word)
        .then(|| {
            normalize_blank_whitespace(block.slice(words[first_prefix_word].start, span.start))
        })
        .unwrap_or_default();
    let suffix = (last_suffix_word > span.last_word)
        .then(|| normalize_blank_whitespace(block.slice(span.end, words[last_suffix_word].end)))
        .unwrap_or_default();
    let prefix = js_regex(
        r"(?iu)^(?:\[[0-9]+\]|\([A-Za-z0-9ivxlcdm]+\)|[0-9]+(?:[.)]|\]))\s*",
        &LEADING_CONTEXT_LABEL,
    )
    .replace(&prefix, "")
    .into_owned();
    (prefix, suffix)
}

fn build_directive(
    block: FragmentText<'_>,
    quote: &str,
    document: FragmentText<'_>,
    page_scoped: bool,
) -> Option<(Vec<String>, usize)> {
    let selected = choose_source_span(block, quote)?;
    let span = adjust_span_edges(block, selected);
    let target = normalize_blank_whitespace(block.slice(span.start, span.end));
    let target_words = quote_words(&target);
    if target_words.is_empty() {
        return None;
    }
    let target_count = directive_match_count(document, &target, "", "");
    let range_required = target.contains('\n');
    let range_preferred = target_words.len() >= 20 || utf16_len(&target) >= 150;
    if range_required || range_preferred {
        if let Some(range) = build_range_directive(block, &span, document) {
            return Some(range);
        }
        if range_required && target_count != 1 {
            return None;
        }
    }
    let needs_context =
        target_words.len() <= 3 || target_count != 1 || page_scoped && target_words.len() <= 8;
    if !needs_context && target_count == 1 {
        return Some((exact_directives(&target, "", ""), span.start));
    }
    for window in [4, 2, 8, 12, 16, 24, 32] {
        let (prefix, suffix) = context_for(block, &span, window);
        for (candidate_prefix, candidate_suffix) in [
            (prefix.as_str(), ""),
            ("", suffix.as_str()),
            (prefix.as_str(), suffix.as_str()),
        ] {
            if (candidate_prefix.is_empty() && candidate_suffix.is_empty())
                || directive_match_count(document, &target, candidate_prefix, candidate_suffix) != 1
            {
                continue;
            }
            return Some((
                exact_directives(&target, candidate_prefix, candidate_suffix),
                span.start,
            ));
        }
    }
    (target_count == 1).then(|| (exact_directives(&target, "", ""), span.start))
}

fn fragment_directives(
    block_text: &str,
    document: FragmentText<'_>,
    quotes: &[String],
    page_scoped: bool,
) -> Vec<String> {
    let block = TextSearch::new(block_text);
    let block = FragmentText::Text(&block);
    let mut keys = HashSet::new();
    let quotes = quotes
        .iter()
        .filter(|quote| {
            let key = quote_words(quote).join(" ");
            !key.is_empty() && keys.insert(key)
        })
        .collect::<Vec<_>>();
    if quotes.is_empty() {
        return Vec::new();
    }
    let mut built = Vec::with_capacity(quotes.len());
    for quote in quotes {
        let Some(directive) = build_directive(block, quote, document, page_scoped) else {
            return Vec::new();
        };
        built.push(directive);
    }
    built.sort_by_key(|(_, start)| *start);
    let mut seen = HashSet::new();
    built
        .into_iter()
        .flat_map(|(directives, _)| directives)
        .filter(|directive| seen.insert(directive.clone()))
        .collect()
}

pub fn text_fragment_directives(
    block_text: &str,
    document_text: Option<&str>,
    quotes: &[String],
    page_scoped: bool,
) -> Vec<String> {
    let document = TextSearch::new(
        document_text
            .filter(|text| !js_trim(text).is_empty())
            .unwrap_or(block_text),
    );
    fragment_directives(
        block_text,
        FragmentText::Text(&document),
        quotes,
        page_scoped,
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
    let words = tokenize_source_text(&block);
    let scalar = ScalarText::new(&block);
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
