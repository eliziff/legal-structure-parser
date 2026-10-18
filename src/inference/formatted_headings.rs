//! Headings a source's own formatting marks.

use super::*;

/// Source formatting supplies isolation that physical text lines cannot. Keep
/// the source paragraph and its anchors; append only a heuristic heading role.
#[cfg(all(feature = "native-markup", feature = "courtlistener"))]
pub(crate) fn supplement_source_formatted_headings(
    structure: &mut DocumentStructure,
    heading_exclusions: &[ScalarRange],
    body_exclusions: &[ScalarRange],
) {
    if structure.provider != "courtlistener" || structure.markup_elements.is_empty() {
        return;
    }
    struct SourceBlock<'a> {
        element: usize,
        native: Option<usize>,
        owner: Option<usize>,
        range: ScalarRange,
        text: &'a str,
        marker: bool,
        candidate: bool,
        heading: bool,
        body: bool,
        author: bool,
        context: bool,
        court_caption: bool,
        docket: bool,
    }
    let merge_ranges = |mut ranges: Vec<ScalarRange>| {
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        let mut merged = Vec::<ScalarRange>::new();
        for range in ranges {
            if range.end <= range.start { continue; }
            if let Some(last) = merged.last_mut().filter(|last| range.start <= last.end) {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        merged
    };
    let overlaps = |range: ScalarRange, others: &[ScalarRange]| {
        others.partition_point(|other| other.start < range.end).checked_sub(1)
            .is_some_and(|index| others[index].end > range.start)
    };
    let notes = structure.nodes.iter()
        .filter(|node| matches!(node.kind, NodeKind::Footnote | NodeKind::Endnote))
        .flat_map(|node| structure.query_ranges(node).into_owned())
        .chain(structure.notes.iter().flat_map(|note| note.label_ranges.iter().chain(&note.body_ranges).copied()))
        .collect::<Vec<_>>();
    let blocked_headings = merge_ranges(heading_exclusions.iter().copied()
        .chain(notes.iter().copied()).collect());
    let blocked_body = merge_ranges(body_exclusions.iter().copied()
        .chain(notes).collect());
    let existing_headings = merge_ranges(structure.nodes.iter()
        .filter(|node| node.kind == NodeKind::Heading)
        .map(|node| structure.query_range(node)).collect());
    let additions = {
        let elements = &structure.markup_elements;
        let coordinates = ScalarText::new(structure.query_text());
        let trimmed = |range: ScalarRange| {
            let raw = coordinates.slice_utf16(range.start..range.end)?;
            let value = trim_javascript_whitespace(raw);
            let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
            let start = range.start + utf16_len(&raw[..leading]);
            Some((ScalarRange { start, end: start + utf16_len(value) }, value))
        };
        let opinion = |tag: &str, attributes: &BTreeMap<String, String>| {
            tag.eq_ignore_ascii_case("opinion")
                || (matches!(tag.to_ascii_lowercase().as_str(), "div" | "article" | "section")
                    && attributes.get("class").is_some_and(|classes| classes.split_ascii_whitespace()
                        .any(|class| matches!(class, "opinion" | "co_opinionBlock"))))
        };
        let excluded = |tag: &str, attributes: &BTreeMap<String, String>| {
            let tag = tag.to_ascii_lowercase();
            (tag != "blockquote" && crate::native_markup::source_heading_exclusion(&tag, attributes))
                || matches!(tag.as_str(), "author" | "footnote" | "footnote_body" | "table"
                    | "ul" | "ol" | "dl" | "li" | "dt" | "dd" | "header" | "footer" | "nav" | "form")
        };
        let external_exclusion = structure.markup_ancestors.iter()
            .any(|ancestor| excluded(&ancestor.tag, &ancestor.attributes));
        let external_quote = structure.markup_ancestors.iter()
            .any(|ancestor| ancestor.tag.eq_ignore_ascii_case("blockquote"));
        let mut native = HashMap::<usize, Option<usize>>::new();
        for (index, node) in structure.nodes.iter().enumerate()
            .filter(|(_, node)| node.source == Derivation::Native) {
            if let Some(start) = node.markup_start {
                native.entry(start).and_modify(|value| *value = None).or_insert(Some(index));
            }
        }
        let mut owners = Vec::<Option<usize>>::with_capacity(elements.len());
        let mut exclusions = Vec::<bool>::with_capacity(elements.len());
        let mut quotations = Vec::<bool>::with_capacity(elements.len());
        let mut paragraphs = Vec::<Option<usize>>::with_capacity(elements.len());
        let mut formats = vec![Vec::<ScalarRange>::new(); elements.len()];
        for (index, element) in elements.iter().enumerate() {
            owners.push(if opinion(&element.tag, &element.attributes) {
                Some(index)
            } else { element.parent.and_then(|parent| owners[parent]) });
            exclusions.push(excluded(&element.tag, &element.attributes)
                || element.parent.map_or(external_exclusion, |parent| exclusions[parent]));
            quotations.push(element.tag.eq_ignore_ascii_case("blockquote")
                || element.parent.map_or(external_quote, |parent| quotations[parent]));
            let paragraph = element.parent.and_then(|parent| paragraphs[parent]);
            // Emphasis, and a centred run (Columbia sets "OPINION" or "Findings of Fact." in CENTER).
            if matches!(element.tag.to_ascii_lowercase().as_str(), "em" | "strong" | "b" | "center") {
                if let Some(paragraph) = paragraph {
                    formats[paragraph].push(element.range);
                }
            }
            paragraphs.push(if element.tag.eq_ignore_ascii_case("p") { Some(index) } else { paragraph });
        }
        static POSSESSIVE: OnceLock<Regex> = OnceLock::new();
        let possessive = POSSESSIVE.get_or_init(|| legal_citations::grammar::compile_ecmascript_table_entry(
            "cue.heading-possessive").expect("shared heading possessive grammar"));
        static TRAILING_DATE: OnceLock<Regex> = OnceLock::new();
        let trailing_date = TRAILING_DATE.get_or_init(|| legal_citations::grammar::compile_ecmascript_table_entry(
            "cue.heading-trailing-date").expect("shared heading date grammar"));
        let bare_marker = |value: &str| {
            !value.contains(char::is_whitespace) && enum_readings(
                value.trim_start_matches('(').trim_end_matches(['.', ')']))
                .into_iter().flatten().next().is_some()
        };
        let title = |value: &str| {
            if bare_marker(value) || utf16_longer(value, 200)
                || value.trim_end_matches([':', '.', ',']).eq_ignore_ascii_case("per curiam")
                || value.ends_with([',', ';']) {
                return false;
            }
            let printed_words = value.split_whitespace().collect::<Vec<_>>();
            // Letter spacing and a glued source enumerator are typography, not
            // changes to the printed title stored in the new node.
            let spaced = printed_words.len() > 1 && printed_words.iter().all(|word|
                word.chars().count() == 1 && word.chars().next().is_some_and(char::is_uppercase));
            let compact;
            let grammar = if spaced {
                compact = printed_words.join("");
                compact.as_str()
            } else { value };
            if bare_marker(grammar) { return false; }
            let joined;
            let grammar = if let Some(point) = grammar.find('.').filter(|point|
                heading_enumerator(&grammar[..=(*point)]) && grammar[point + 1..]
                    .chars().next().is_some_and(char::is_uppercase)) {
                joined = format!("{} {}", &grammar[..=point], &grammar[point + 1..]);
                joined.as_str()
            } else { grammar };
            let words = grammar.split_whitespace().collect::<Vec<_>>();
            let enumerated = words.first().is_some_and(|word| heading_enumerator(word));
            // A printed full stop remains in the role's range and label.
            let grammar = if words.len() > 1 { grammar.strip_suffix('.').unwrap_or(grammar) } else { grammar };
            let grammar = trim_leading_parenthetical(grammar);
            let title_text = if enumerated {
                grammar.split_once(char::is_whitespace).map_or(grammar, |(_, rest)| rest.trim_start())
            } else { grammar };
            let title_text = if enumerated {
                title_text.trim_matches(['"', '\u{201c}', '\u{201d}']).trim_end_matches('.')
            } else { title_text };
            let citation_text = possessive.replace_all(title_text, "$1");
            let analysis = &crate::analysis::STRUCTURE_ANALYSIS;
            // Keep the shared dated-title guard: an argument/filed date is
            // metadata, even when source typography capitalizes its words.
            if !enumerated && grammar.chars().last().is_some_and(|character| character.is_ascii_digit())
                && !analysis.heading_text_plausible(grammar) { return false; }
            let uppercase = title_text.chars().filter(|character| character.is_alphabetic())
                .all(char::is_uppercase);
            // An isolated enumerated issue title can name the statute at issue;
            // a capitalized name initial alone is not a reporter/neutral cite.
            (enumerated || ((!analysis.has_citation_cue(&citation_text) || uppercase)
                    && !analysis.has_citation_signal(&citation_text)))
                && ((heading_level(&title_text.split_whitespace().collect::<Vec<_>>(), enumerated)
                        && (enumerated || heading_title_case(&words)))
                    || analysis.heading_text_plausible(grammar))
                && !(!enumerated && value.ends_with('.') && words.len() == 1)
        };
        let separator = |value: &str| value.chars().all(|character|
            javascript_whitespace(character) || matches!(character, '-' | '\u{2013}' | '\u{2014}' | '_' | '*'));
        let attribution = cached_regex!(SOURCE_HEADING_ATTRIBUTION,
            r"(?i)^(?:by\s+the\s+court\b|per\s+curiam\b|before\b.*\b(?:justices?|judges?)\b)|\bopinion\s+by\b|\b(?:chief\s+justice|chief\s+judge)\b|,\s*(?:j|jj|[cp]\.?j)\.,?\s*(?:concur|dissent|delivered|filed|joined|not\s+participating)");
        let caption = cached_regex!(SOURCE_HEADING_CAPTION,
            r"(?i)^(?:in\s+the(?:\s|$)|from\s+the\b|in\s+re\b|the\s+state\s+of\b|original\s+proceeding\b)|^(?:affirmed|reversed|vacated|dismissed|remanded|so\s+ordered)(?:\s+in\s+part)?[.!]?$|\b(?:courts?|district|circuit)\s+(?:of|court)\b|\bappeals?\s+from\b|\b(?:appellants?|appellees?|petitioners?|respondents?|plaintiffs?|defendants?)\b|\bcounty(?:,|\s*$)");
        let court_caption = cached_regex!(SOURCE_HEADING_COURT_CAPTION,
            r"(?i)\b(?:courts?|district|circuit)\s+(?:of|court)\b");
        let docket = cached_regex!(SOURCE_HEADING_DOCKET,
            r"(?i)^(?:no\.|number)\s*\S*\d|\b(?:court|case|cause|docket)\s+(?:nos?\.?|number)\b");
        let mut body_seen = HashSet::<Option<usize>>::new();
        let mut attachments = HashSet::<Option<usize>>::new();
        let mut blocks = Vec::<SourceBlock<'_>>::new();
        for (index, element) in elements.iter().enumerate() {
            let tag = element.tag.to_ascii_lowercase();
            let native_index = native.get(&element.markup_start).copied().flatten();
            let node = native_index.map(|index| &structure.nodes[index]);
            if !matches!(tag.as_str(), "p" | "blockquote" | "author" | "table" | "ul" | "ol" | "dl")
                && !node.is_some_and(|node| matches!(node.kind, NodeKind::Heading | NodeKind::Footnote | NodeKind::Endnote)) {
                continue;
            }
            let Some((range, value)) = trimmed(element.range) else { continue; };
            if value.is_empty() { continue; }
            let mut cursor = range.start;
            for format in merge_ranges(std::mem::take(&mut formats[index])) {
                if format.end <= cursor { continue; }
                if format.start > cursor && coordinates.slice_utf16(cursor..format.start)
                    .is_none_or(|gap| !trim_javascript_whitespace(gap).is_empty()) { break; }
                cursor = cursor.max(format.end.min(range.end));
            }
            let formatted = tag == "p" && cursor >= range.end;
            let heading = overlaps(range, &existing_headings);
            let prose = node.is_some_and(|node| matches!(node.kind, NodeKind::Prose | NodeKind::Paragraph));
            let enumerated = value.split_whitespace().next().is_some_and(heading_enumerator);
            let front = !body_seen.contains(&owners[index]);
            let authored = tag == "author" || (utf16_len(value) <= 300 && attribution.is_match(value));
            // Source-only roles remain retained. Caption and authorship text
            // may separate a decision title from its body, but cannot become
            // the body evidence that validates another formatted title.
            let metadata = front && !enumerated && utf16_len(value) <= 200
                && (caption.is_match(value) || docket.is_match(value)
                    || (trailing_date.is_match(value) && !title(value))
                    || (value.chars().filter(|character| character.is_alphabetic()).all(char::is_uppercase)
                        && (value.contains(',') || value.contains(" - "))));
            let attachment = !quotations[index] && !exclusions[index]
                && value.split_whitespace().next().is_some_and(|word|
                    word.trim_end_matches(':').eq_ignore_ascii_case("appendix"));
            // Blank-answer tables declare a compound form region; its own
            // legal headings need a separate role reader, not borrowed reasons.
            if attachment || (tag == "table" && !quotations[index] && value.contains("Yes No")
                && value.match_indices("___").take(2).count() == 2) {
                attachments.insert(owners[index]);
            }
            let source_body = prose && !exclusions[index] && !overlaps(range, &blocked_body)
                && !attachments.contains(&owners[index]);
            let candidate = formatted && source_body && !authored && !metadata && !quotations[index]
                && !overlaps(range, &blocked_headings) && title(value);
            let body = source_body && !candidate && !authored && !metadata
                && value.split_whitespace().filter(|word|
                    word.chars().next().is_some_and(char::is_lowercase)
                        && word.chars().filter(|character| character.is_alphabetic()).take(3).count() == 3)
                    .take(2).count() == 2;
            if body { body_seen.insert(owners[index]); }
            blocks.push(SourceBlock {
                element: index, native: native_index, owner: owners[index], range, text: value,
                marker: tag == "p" && source_body && !quotations[index]
                    && !overlaps(range, &blocked_headings) && bare_marker(value),
                candidate,
                heading, body, author: authored && prose && !overlaps(range, &blocked_body),
                // Short source attribution/introductory paragraphs can sit
                // between a title and its body. They supply no heading role.
                context: !body && source_body && !quotations[index] && (metadata || bare_marker(value) || separator(value)
                    || (!formatted && value.chars().any(char::is_lowercase) && title(value))),
                court_caption: metadata && source_body && court_caption.is_match(value),
                docket: metadata && source_body && docket.is_match(value),
            });
        }
        // A court caption can continue with an isolated location before its
        // docket. Use that source relationship rather than a city-name table.
        for index in 1..blocks.len().saturating_sub(1) {
            let (before, block, after) = (&blocks[index - 1], &blocks[index], &blocks[index + 1]);
            if block.candidate && before.court_caption && after.docket
                && before.owner == block.owner && after.owner == block.owner
                && elements[before.element].parent == elements[block.element].parent
                && elements[after.element].parent == elements[block.element].parent
                && block.text.split_whitespace().count() <= 4
                && block.text.chars().filter(|character| character.is_alphabetic()).all(char::is_uppercase) {
                blocks[index].candidate = false;
                blocks[index].context = true;
            }
        }
        // The source may print a heading's enumerator in a separate paragraph.
        // Group its literal span with the adjacent title, retaining both native
        // paragraphs and their shared source parent without inferring a rank.
        let mut groups = vec![None; blocks.len()];
        let mut grouped_titles = vec![false; blocks.len()];
        for (index, pair) in blocks.windows(2).enumerate() {
            let (marker, title) = (&pair[0], &pair[1]);
            if !marker.marker || !title.candidate || marker.owner != title.owner
                || title.text.split_whitespace().next().is_some_and(heading_enumerator)
                || !enum_readings(marker.text.trim_start_matches('(').trim_end_matches(['.', ')']))
                    .into_iter().flatten().any(|(family, _)| family != 0)
                || elements[marker.element].parent != elements[title.element].parent
                || marker.native.and_then(|index| structure.nodes[index].parent_id.as_deref())
                    != title.native.and_then(|index| structure.nodes[index].parent_id.as_deref())
                || coordinates.slice_utf16(marker.range.end..title.range.start)
                    .is_none_or(|gap| !trim_javascript_whitespace(gap).is_empty()) { continue; }
            grouped_titles[index + 1] = true;
            let range = ScalarRange { start: marker.range.start, end: title.range.end };
            if !overlaps(range, &existing_headings) && !overlaps(range, &blocked_headings) {
                groups[index] = Some(range);
            }
        }
        let mut follows_body = vec![false; blocks.len()];
        for index in (0..blocks.len()).rev() {
            let block = &blocks[index];
            let next = blocks.get(index + 1).is_some_and(|next| next.owner == block.owner
                && coordinates.slice_utf16(block.range.end..next.range.start)
                    .is_some_and(separator)
                && follows_body[index + 1]);
            follows_body[index] = block.body
                || ((block.candidate || block.heading || block.author || block.context || groups[index].is_some()) && next);
        }
        let mut ordinal = structure.nodes.iter().filter_map(|node|
            node.id.strip_prefix("heuristic-heading-")?.parse::<usize>().ok()).max().unwrap_or(0);
        let mut result = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            if (!block.candidate && groups[index].is_none()) || block.heading
                || !follows_body[index] || grouped_titles[index] { continue; }
            let (range, native) = groups[index].map_or((block.range, block.native), |range|
                (range, blocks[index + 1].native));
            let Some(label) = coordinates.slice_utf16(range.start..range.end) else { continue; };
            ordinal += 1;
            let mut node = StructureNode::new(format!("heuristic-heading-{ordinal:06}"), NodeKind::Heading,
                range, ENGINE_ORIGIN, Derivation::Heuristic,
                native.and_then(|index| structure.nodes[index].parent_id.clone()));
            node.label = Some(label.to_owned());
            result.push(node);
        }
        result
    };
    structure.nodes.extend(additions);
}
