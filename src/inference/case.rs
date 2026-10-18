//! A case's reading: its paragraphs with their notes, its source layout, headings and prose.

use super::*;

pub(super) fn detect_case(
    text: &ScalarText<'_>,
    profile: DetectionProfile,
    report_start_page: Option<u32>,
    require_report_start: bool,
    excluded: &[ScalarRange],
    courtlistener: bool,
) -> Vec<Block> {
    let mut paragraphs = match profile {
        DetectionProfile::CaseContiguousComplete => {
            let complete = clipped_case_paragraphs(
                detect_paragraphs(text, DetectionProfile::CaseLossy, excluded, courtlistener),
                excluded,
                text,
            );
            if !complete.is_empty() && !gapped_paragraphs(&complete) {
                complete
            } else {
                detect_paragraphs(text, DetectionProfile::CaseContiguousComplete, excluded, courtlistener)
            }
        }
        profile => detect_paragraphs(text, profile, excluded, courtlistener),
    };
    if profile == DetectionProfile::CaseRootedComplete {
        paragraphs = with_endnotes(text, paragraphs);
        for block in paragraphs.iter_mut().filter(|block| block.kind == NodeKind::Paragraph) {
            block.range.end = paragraph_end_before_signatures(text, block.range.start, block.range.end);
        }
        if !paragraphs.iter().any(|block| block.kind == NodeKind::Paragraph) {
            let until = paragraphs.iter().filter(|block| block.kind == NodeKind::Endnote)
                .filter_map(|block| block.marker_range.map(|range| range.start)).min().unwrap_or(text.len());
            paragraphs.extend(unnumbered_paragraphs(text, until));
        }
    }
    paragraphs.extend(detect_pages(text, report_start_page, require_report_start));
    paragraphs
}

/// A provider text's paragraphs with its note list after them, paired by note pairing's plain-text
/// mode: the list may open at the last "[1] …" or "Footnote 1" line after the paragraphs' last
/// number (with a "Footnotes" heading just above it), and the paragraphs' numbered lines are labels
/// the pairer may take for notes instead. Where it pairs notes, the paragraphs end where they open.
pub(super) fn with_endnotes(text: &ScalarText<'_>, mut paragraphs: Vec<Block>) -> Vec<Block> {
    let after = paragraphs.iter().map(|block| block.range.start).max().unwrap_or(0);
    let index = text.lines();
    let opening = index.iter().rposition(|line| {
        let value = text.value[line[0]..line[1]].trim();
        line[2] > after && (value.starts_with("[1]") || value == "Footnote 1")
    });
    let Some(opening) = opening else { return paragraphs };
    let heading = opening.checked_sub(1).filter(|previous| {
        matches!(text.value[index[*previous][0]..index[*previous][1]].trim(),
            "Footnotes" | "FOOTNOTES" | "Notes" | "NOTES" | "Endnotes" | "ENDNOTES")
    });
    let note_start = index[opening][2];
    let labels = paragraphs.iter().filter_map(|block| block.marker_range.map(|range| range.start))
        .map(|start| index[index.partition_point(|line| line[2] <= start) - 1][2])
        .collect::<HashSet<_>>();
    let notes = crate::text_notes::text_note_blocks(text, note_start, &labels);
    if notes.is_empty() {
        return paragraphs;
    }
    let list_start = heading.map_or(note_start, |line| index[line][2]);
    for block in &mut paragraphs {
        if block.range.end > list_start {
            block.range.end = list_start.max(block.range.start);
        }
    }
    paragraphs.extend(notes);
    paragraphs
}

#[derive(Default)]
pub(super) struct CaseSourceLayout {
    pub(super) blocks: Vec<Block>,
    pub(super) exclusions: Vec<ScalarRange>,
    note_ranges: Vec<ScalarRange>,
    running_heads: HashSet<usize>,
    /// Margin line numerals: never paragraph or note markers, and not part of the lines they number.
    pub(super) gutter: Vec<ScalarRange>,
}

pub(super) struct SourcePageMarker {
    marker: Marker,
    range: ScalarRange,
    line: ScalarRange,
    after_line: usize,
    physical: ScalarRange,
    header: bool,
}

pub(super) fn case_decimal_subheadings(
    evidence: &DocumentInput,
    text: &ScalarText<'_>,
    source_lines: &[Line<'_>],
    numeric: &[Marker],
) -> Vec<Block> {
    let markers = numeric.iter().filter(|marker| marker.style == MarkerStyle::Dot)
        .map(|marker| (marker.start, marker)).collect::<HashMap<_, _>>();
    let mut parent = None;
    let mut families = BTreeMap::<usize, Vec<Marker>>::new();
    let mut supported = HashSet::new();
    let mut titles = HashMap::<usize, ScalarRange>::new();
    for (index, line) in source_lines.iter().enumerate() {
        let value = line.text.trim();
        let Some(token) = value.split_whitespace().next() else { continue; };
        let lead = line.text.len() - line.text.trim_start().len();
        let mut range = ScalarRange { start: text.scalar(line.byte_start + lead), end: text.scalar(line.byte_start + lead + value.len()) };
        if evidence.clip_inference(EvidenceKind::Heading, range) != Some(range) { continue; }
        if heading_enumerator(token) && heading_levels(value).is_some()
            && heading_title_case(&value.split_whitespace().skip(1).collect::<Vec<_>>()) {
            let inner = token.trim_start_matches('(').trim_end_matches(['.', ')']);
            if enum_readings(inner).into_iter().flatten().any(|(family, _)| family != 0) {
                parent = Some(line.scalar_start);
                continue;
            }
        }
        let Some(parent) = parent else { continue; };
        let Some(&marker) = markers.get(&line.scalar_start) else { continue; };
        if !decimal_prefix(value, 4).is_some_and(|(_, length)| value[length..].starts_with('.')) { continue; }
        let mut joined = value.to_owned();
        for continuation in &source_lines[index + 1..] {
            let value = continuation.text.trim();
            let words = value.split_whitespace().collect::<Vec<_>>();
            if leading_ascii_space(continuation.text) <= leading_ascii_space(line.text)
                || !value.chars().any(char::is_alphabetic) || !heading_title_case(&words)
                || words.first().is_some_and(|word| heading_enumerator(word)) { break; }
            let next = joined.split_whitespace().chain(words.iter().copied()).collect::<Vec<_>>().join(" ");
            if heading_levels(&next).is_none() { break; }
            let lead = continuation.text.len() - continuation.text.trim_start().len();
            let extended = ScalarRange { start: range.start,
                end: text.scalar(continuation.byte_start + lead + value.len()) };
            if evidence.clip_inference(EvidenceKind::Heading, extended) != Some(extended) { break; }
            // A hanging title line retains its literal source span; soft body wraps
            // do not acquire heading status from title grammar alone.
            joined = next;
            range = extended;
        }
        let following = source_lines[index + 1..].iter().find(|line|
            line.scalar_start >= range.end && !line.text.trim().is_empty());
        let title = text.slice(marker.content_start..range.end).unwrap_or("").trim();
        let words = title.split_whitespace().collect::<Vec<_>>();
        if heading_levels(&joined).is_some() && title.chars().any(char::is_alphabetic)
            && (heading_title_case(&words) || title.ends_with('?')
                || following.is_some_and(|next| sentence_heading(title, next.text))) {
            supported.insert(marker.start);
        }
        titles.insert(marker.start, range);
        families.entry(parent).or_default().push(marker.clone());
    }
    let mut result = Vec::new();
    for family in families.values() {
        for scope in monotone_scopes(family, 1) {
            let corroborated = scope.iter().filter(|marker| supported.contains(&marker.start)).count();
            if scope.len() < 2 || scope[0].number != 1 || corroborated < 2 || corroborated * 2 < scope.len() { continue; }
            let separated = scope.windows(2).filter(|pair| {
                let body = text.slice(titles[&pair[0].start].end..pair[1].start).unwrap_or("");
                word_count(body, true) >= 12
            }).flat_map(|pair| [pair[0].start, pair[1].start]).collect::<HashSet<_>>();
            let bracketed = scope.windows(3).filter(|triple| {
                if supported.contains(&triple[1].start) || !supported.contains(&triple[0].start)
                    || !supported.contains(&triple[2].start) || !triple.windows(2).all(|pair|
                        word_count(text.slice(titles[&pair[0].start].end..pair[1].start).unwrap_or(""), true) >= 12) { return false; }
                let column = titles[&triple[0].start].start - triple[0].start;
                if !triple.iter().all(|marker| titles[&marker.start].start - marker.start == column) { return false; }
                let title = text.slice(titles[&triple[1].start].start..titles[&triple[1].start].end).unwrap_or("");
                if title.contains('\n') {
                    heading_levels(&title.split_whitespace().collect::<Vec<_>>().join(" ")).is_some()
                } else { heading_levels(title).is_some() }
            }).map(|triple| triple[1].start).collect::<HashSet<_>>();
            for marker in scope.into_iter().filter(|marker|
                (supported.contains(&marker.start) || bracketed.contains(&marker.start))
                    && separated.contains(&marker.start)) {
                let range = titles[&marker.start];
                let title = text.slice(range.start..range.end).unwrap().trim().to_owned();
                result.push(Block::labelled(NodeKind::Heading, title, range.start, range.end));
            }
        }
    }
    result
}

pub(super) fn case_source_layout(evidence: &DocumentInput, text: &ScalarText<'_>) -> CaseSourceLayout {
    let mut result = CaseSourceLayout::default();
    if evidence.provider != "courtlistener" {
        return result;
    }
    let source_lines = lines(text).map(|line| case_source_line(text, line)).collect::<Vec<_>>();
    let after_line = |line: Line<'_>| {
        let end = line.byte_end;
        let end = if text.value.as_bytes().get(end..end + 2) == Some(b"\r\n") {
            end + 2
        } else if text.value.as_bytes().get(end) == Some(&b'\n') {
            end + 1
        } else { end };
        text.scalar(end)
    };
    let form_feeds = text.value.char_indices()
        .filter(|(_, character)| *character == '\u{c}')
        .map(|(byte, _)| text.scalar(byte))
        .collect::<Vec<_>>();
    let physical_page = |start: usize| {
        let at = form_feeds.partition_point(|boundary| *boundary < start);
        ScalarRange {
            start: at.checked_sub(1).map_or(0, |index| form_feeds[index] + 1),
            end: form_feeds.get(at).copied().unwrap_or(text.len()),
        }
    };
    let mut body_indents = source_lines.iter()
        .filter(|line| word_count(line.text, true) >= 8)
        .map(|line| leading_ascii_space(line.text))
        .collect::<Vec<_>>();
    let body_indent = median(&mut body_indents) as usize;
    let mut first_lines = HashSet::new();
    let mut running = HashMap::<&str, Vec<usize>>::new();
    for line in &source_lines {
        let value = line.text.trim();
        if value.is_empty() { continue; }
        let physical = physical_page(line.scalar_start);
        if first_lines.insert(physical.start) && physical.start > 0 {
            running.entry(value).or_default().push(line.scalar_start);
        }
    }
    for starts in running.into_values().filter(|starts| starts.len() >= 3) {
        result.running_heads.extend(starts);
    }
    let numeric = paragraph_markers(text, false, true);
    let line_by_start = source_lines.iter().enumerate().map(|(index, line)|
        (line.scalar_start, index)).collect::<HashMap<_, _>>();
    let mut margin_numbers = HashSet::new();
    let mut ruled = Vec::new();
    let alone =|marker: &Marker| source_lines[line_by_start[&marker.start]].text.trim().bytes().all(|byte| byte.is_ascii_digit());
    for scope in monotone_scopes(numeric.iter().filter(|marker| marker.style == MarkerStyle::Bare), 1) {
        for window in scope.windows(3) {
            let positions = [line_by_start[&window[0].start], line_by_start[&window[1].start], line_by_start[&window[2].start]];
            if positions.windows(2).all(|pair| pair[1] == pair[0] + 1)
                && positions.windows(2).all(|pair|
                    leading_ascii_space(source_lines[pair[0]].text) == leading_ascii_space(source_lines[pair[1]].text))
                && window.iter().any(alone) {
                // Consecutive +1 numerals at one source column number PDF lines,
                // including blank lines; a remote attached numeral is not a note reference to them.
                margin_numbers.extend(window.iter().map(|marker| marker.start));
            }
        }
        // Pleading paper numbers its ruled lines, so text extracted around them can put a
        // line between two numerals.
        let mut run = 0;
        for index in 1..=scope.len() {
            let continues = scope.get(index).is_some_and(|marker| {
                let [left, right] = [line_by_start[&scope[index - 1].start], line_by_start[&marker.start]];
                right > left && right - left <= 2
                    && leading_ascii_space(source_lines[left].text) == leading_ascii_space(source_lines[right].text)
            });
            if continues { continue; }
            if index - run >= 4 && scope[run..index].iter().any(alone) {
                ruled.push((scope[run..index].to_vec(), leading_ascii_space(source_lines[line_by_start[&scope[run].start]].text)));
            }
            run = index;
        }
    }
    // A page's gutter counts from its first line: a long run from 1, many standing alone, is one,
    // and so is a run of five at that gutter's column. A run of note labels starts where its notes do.
    let gutters = ruled.iter().filter(|(run, _)| run[0].number <= 2 && run.len() >= 8
        && run.iter().filter(|marker| alone(marker)).count() >= 3)
        .map(|(_, column)| *column).collect::<HashSet<_>>();
    for (run, column) in &ruled {
        if gutters.contains(column) && run.len() >= 5 {
            margin_numbers.extend(run.iter().map(|marker| marker.start));
        }
    }
    // A gutter numeral is not the text's: the line it numbers keeps its words.
    for marker in numeric.iter().filter(|marker| margin_numbers.contains(&marker.start)) {
        let line = source_lines[line_by_start[&marker.start]];
        let value = text.slice(marker.start..text.scalar(line.byte_end)).unwrap_or("");
        let lead = leading_ascii_space(value);
        let digits = value[lead..].bytes().take_while(u8::is_ascii_digit).count();
        result.gutter.push(ScalarRange { start: marker.start, end: marker.start + lead + digits });
    }
    let decimal_headings = case_decimal_subheadings(evidence, text, &source_lines, &numeric);
    for scope in monotone_scopes(numeric.iter().filter(|marker| marker.style == MarkerStyle::Dot), 1) {
        if scope.len() < 5 || scope[0].number != 1 || scope[0].start > text.len() / 5 { continue; }
        let earlier_prose = source_lines.iter().take_while(|line| line.scalar_start < scope[0].start)
            .filter(|line| line.text.split_whitespace().filter(|word|
                word.chars().next().is_some_and(char::is_lowercase)).take(3).count() == 3)
            .filter(|line| line.text.trim_end().ends_with(['.', '?', '!']))
            .map(|line| line.text.chars().count()).sum::<usize>();
        if earlier_prose > 120 { continue; }
        let topical = scope.iter().filter(|marker| {
            let byte = text.byte(marker.content_start);
            let line = text.value[byte..].split('\n').next().unwrap_or("");
            let Some((topic, _)) = line.split_once('.') else { return false; };
            let words = topic.split_whitespace().collect::<Vec<_>>();
            topic.contains(':') && heading_levels(topic).is_some() && heading_title_case(&words)
        }).count();
        if topical < 3 || topical * 2 <= scope.len() { continue; }
        // A repeated early editorial topic catalogue does not number judicial paragraphs.
        // Keep its source and native declarations; suppress only the unsupported role at markers.
        for marker in scope {
            let at = source_lines.partition_point(|line| line.scalar_start <= marker.start);
            if let Some(line) = at.checked_sub(1).map(|index| source_lines[index]) {
                result.exclusions.push(ScalarRange { start: marker.start, end: text.scalar(line.byte_end) });
            }
        }
    }
    let mut pages = Vec::<SourcePageMarker>::new();
    for line in &source_lines {
        let value = line.text.trim();
        let lead = line.text.len() - line.text.trim_start().len();
        let mut start = text.scalar(line.byte_start + lead);
        let end = text.scalar(line.byte_start + lead + value.len());
        let wrapped = value.starts_with('-') && value.ends_with('-');
        let number = value.trim_matches('-').trim();
        let explicit = value.rfind("Page ").and_then(|offset| {
            let rest = &value[offset + 5..];
            let (number, length) = decimal_prefix(rest, 4)?;
            let total = rest[length..].strip_prefix(" of ")?;
            let (_, digits) = decimal_prefix(total, 4)?;
            (digits == total.len()).then_some((offset, number))
        });
        let Some(number) = explicit.map(|(_, number)| number).or_else(|| {
            decimal_prefix(number, 4).and_then(|(number, length)|
                (length == value.trim_matches('-').trim().len()).then_some(number))
        }) else { continue; };
        let physical = physical_page(start);
        let before = text.slice(physical.start..line.scalar_start).unwrap_or("");
        let line_end = text.scalar(line.byte_end);
        let after = text.slice(line_end..physical.end).unwrap_or("");
        let header = before.trim().is_empty();
        let footer = after.trim().is_empty();
        if let Some((offset, _)) = explicit {
            // A combined administrative footer is supported at the physical boundary;
            // an incidental prose mention of Page N of M does not supply this evidence.
            // A court's filing stamp ("Case 1:20-cv-123 Document 45 Filed 01/02/20 Page 3 of 9")
            // heads each filed page as well as footing it.
            let stamp = value.starts_with("Case ") && value.contains(" Document ") && value.contains(" Filed ");
            if offset > 0 && (form_feeds.is_empty() || !(footer || stamp && header)) { continue; }
            start = text.scalar(line.byte_start + lead + offset);
        }
        let centered = leading_ascii_space(line.text) >= body_indent + 12;
        // Boundary position and a repeated counter support roles; form feeds alone do not.
        if !(explicit.is_some() || centered || wrapped && (header || footer)) {
            continue;
        }
        if !form_feeds.is_empty() && !(header || footer) { continue; }
        pages.push(SourcePageMarker {
            marker: Marker { number: number.parse().unwrap(), start, content_start: end,
                style: MarkerStyle::Bare, score: 1.0, formal: false, sentence: false },
            range: ScalarRange { start, end },
            line: ScalarRange { start: line.scalar_start, end: line_end },
            after_line: after_line(*line),
            physical,
            header: !form_feeds.is_empty() && header && !footer,
        });
    }
    let page_by_start = pages.iter().map(|page| (page.marker.start, page)).collect::<HashMap<_, _>>();
    let mut accepted_pages = Vec::new();
    for header in [false, true] {
        for scope in monotone_scopes(pages.iter().filter(|page| page.header == header).map(|page| &page.marker), 1) {
            if scope.len() < 3 {
                if scope.len() != 2 || form_feeds.is_empty() || header { continue; }
                let left = page_by_start[&scope[0].start];
                let right = page_by_start[&scope[1].start];
                if left.physical == right.physical || [left, right].iter().any(|page|
                    form_feeds.binary_search(&page.physical.end).is_err())
                    || word_count(text.slice(left.after_line..right.line.start).unwrap_or(""), true) < 80 { continue; }
            }
            let mut scope = scope;
            // A filing stamp's numbering yields to the document's own printed pages: it reads only where
            // they read none (an exhibit stamped in another court keeps the record's page numbers).
            if header && accepted_pages.iter().any(|page: &&SourcePageMarker| !page.header)
                && scope.iter().all(|marker| text.slice(page_by_start[&marker.start].line.start..page_by_start[&marker.start].line.end)
                    .is_some_and(|line| line.contains(" Document ") && line.contains(" Filed "))) {
                continue;
            }
            if form_feeds.is_empty() {
                // Without page breaks a page holds a page's words: the counters stand as far as
                // that holds, and a stray short final run does not unseat the pages before it.
                let length = scope.len();
                let held = scope.windows(2).enumerate().take_while(|(position, pair)| {
                    let left = page_by_start[&pair[0].start];
                    let right = page_by_start[&pair[1].start];
                    let words = word_count(text.slice(left.line.end..right.line.start).unwrap_or(""), true);
                    words >= 80 || position + 2 == length && words >= 12
                        && left.range.start - left.line.start == right.range.start - right.line.start
                        && text.slice(right.after_line..text.len()).unwrap_or("").trim().is_empty()
                }).count() + 1;
                if held < length && held < 3 { continue; }
                scope.truncate(held);
            }
            for (position, marker) in scope.iter().enumerate() {
                let page = page_by_start[&marker.start];
                result.exclusions.push(page.line);
                accepted_pages.push(page);
                let (start, end) = if form_feeds.is_empty() {
                    let Some(previous) = position.checked_sub(1).map(|index| page_by_start[&scope[index].start]) else {
                        // An initial footer above 1 may follow an unmarked earlier page.
                        if marker.number != 1 { continue; }
                        let end = page.line.start;
                        if end == 0 { continue; }
                        let mut block = Block::labelled(NodeKind::Page, marker.number.to_string(), 0, end);
                        block.aliases.push(marker.number.to_string());
                        block.marker_range = Some(page.range);
                        result.blocks.push(block);
                        continue;
                    };
                    (previous.after_line, page.line.start)
                } else if page.header {
                    (page.after_line.min(page.physical.end), page.physical.end)
                } else {
                    (page.physical.start, page.line.start)
                };
                if start >= end { continue; }
                let mut block = Block::labelled(NodeKind::Page, marker.number.to_string(), start, end);
                block.aliases.push(marker.number.to_string());
                block.marker_range = Some(page.range);
                result.blocks.push(block);
            }
        }
    }
    accepted_pages.sort_unstable_by_key(|page| page.range.start);
    let mut references = HashMap::<u32, Vec<ScalarRange>>::new();
    for (byte, _) in text.value.char_indices().filter(|(_, character)| character.is_ascii_digit()) {
        let Some(previous) = text.value[..byte].chars().next_back() else { continue; };
        if !(previous.is_alphabetic() || ".,;:!?\"'\u{201d}\u{2019})]".contains(previous)) {
            continue;
        }
        if previous.is_alphabetic() && text.value[..byte].chars().rev()
            .take_while(|character| character.is_alphabetic()).take(3).count() < 3 {
            continue;
        }
        let Some((number, length)) = decimal_prefix(&text.value[byte..], 3) else { continue; };
        if !text.value[byte + length..].chars().next().is_some_and(char::is_whitespace) { continue; }
        // Citation note pins do not independently establish this document's footnote reference.
        let prefix = text.value[..byte].split_whitespace().next_back().unwrap_or("");
        if matches!(prefix, "n." | "nn.") { continue; }
        references.entry(number.parse().unwrap()).or_default().push(ScalarRange {
            start: text.scalar(byte), end: text.scalar(byte + length),
        });
    }
    let declared_paragraph_markers = evidence.native_claims.iter()
        .filter(|claim| claim.kind == EvidenceKind::Paragraph)
        .filter_map(|claim| Some((claim.range.start, claim.label.as_deref()?.strip_prefix("par")?.parse::<u32>().ok()?)))
        .collect::<HashSet<_>>();
    let note_markers = numeric.into_iter()
        .filter(|marker| marker.style == MarkerStyle::Bare)
        // An explicit paragraph number cannot also declare a guessed footnote.
        .filter(|marker| !declared_paragraph_markers.contains(&(marker.start, marker.number)))
        .filter(|marker| !margin_numbers.contains(&marker.start))
        .filter(|marker| !accepted_pages.iter().any(|page| page.line.start <= marker.start && marker.start <= page.line.end))
        .collect::<Vec<_>>();
    let mut labels = HashMap::<u32, usize>::new();
    for marker in &note_markers { *labels.entry(marker.number).or_default() += 1; }
    let supported_references = note_markers.iter().filter_map(|marker| {
        let physical = physical_page(marker.start);
        let prior = references.get(&marker.number).into_iter().flatten()
            .filter(|reference| reference.end <= marker.start).copied().collect::<Vec<_>>();
        let local = prior.iter().filter(|reference| physical.start <= reference.start).copied().collect::<Vec<_>>();
        let reference = if local.len() == 1 {
            Some(local[0])
        } else if local.is_empty() && labels[&marker.number] == 1 && prior.len() == 1 {
            Some(prior[0])
        } else { None };
        reference.map(|reference| (marker.start, reference))
    }).collect::<HashMap<_, _>>();
    for marker in &note_markers {
        let at = source_lines.partition_point(|line| line.scalar_start <= marker.start);
        let Some(line) = at.checked_sub(1).map(|index| source_lines[index]) else { continue; };
        let value = line.text.trim();
        let Some((number, length)) = decimal_prefix(value, 3) else { continue; };
        // Standalone labels have independent source-layout evidence. Corroborated
        // adjacent inline labels can bound a fragment without assigning a note role.
        if value.len() != length { continue; }
        let label_start = text.scalar(line.byte_start + line.text.len() - line.text.trim_start().len());
        let label_range = ScalarRange { start: label_start, end: label_start + length };
        let physical = physical_page(label_start);
        let Some(&reference) = supported_references.get(&marker.start) else { continue; };
        let body_start = after_line(line);
        let mut body_end = physical.end;
        if let Some(next) = note_markers.iter().find(|next| next.start > marker.start
            && next.number == marker.number + 1 && supported_references.contains_key(&next.start)) {
            body_end = body_end.min(next.start);
        }
        // Even a footer in a short counter sequence bounds the physical fragment.
        if let Some(page) = pages.iter().find(|page| !page.header && page.line.start > marker.start
            && page.physical == physical) {
            body_end = body_end.min(page.line.start);
        }
        let (mut body_seen, mut blanks, mut blank_start, mut separate_block) = (false, 0, 0, false);
        let fragment_limit = body_end;
        for following in source_lines.iter().skip(at).take_while(|line| line.scalar_start < fragment_limit) {
            if following.text.trim().is_empty() {
                if blanks == 0 { blank_start = following.scalar_start; }
                blanks += 1;
                if body_seen && blanks >= 2 {
                    body_end = blank_start;
                    separate_block = true;
                    break;
                }
            } else {
                body_seen = true;
                blanks = 0;
            }
        }
        if body_start >= body_end || word_count(text.slice(body_start..body_end).unwrap_or(""), true) < 3
            || !source_lines.iter().skip(at).take_while(|line| line.scalar_start < body_end)
                .find(|line| !line.text.trim().is_empty())
                .is_some_and(|line| line.text.chars().any(char::is_alphabetic)) { continue; }
        let body_range = ScalarRange { start: body_start, end: body_end };
        if evidence.clip_inference(EvidenceKind::Footnote, body_range) != Some(body_range) { continue; }
        let mut block = Block::labelled(NodeKind::Footnote, format!("fn{number}"), body_start, body_end);
        block.marker_range = Some(label_range);
        block.note = Some(Note {
            id: String::new(), node_id: None, kind: NoteKindV2::Footnote,
            label_range, body_range, label_ranges: vec![label_range], body_ranges: vec![body_range],
            references: vec![NoteReference { range: reference, anchor: None, page_indexes: Vec::new(), line_ids: Vec::new() }],
            primary_reference: Some(reference),
        });
        if separate_block {
            block.diagnostic = Some("source-note-block-fragment");
        } else if physical.end < text.len() && (body_end == physical.end
            || pages.iter().any(|page| page.line.start == body_end)) {
            block.diagnostic = Some("source-note-page-fragment");
        }
        // A later separate block on the note's physical page is unresolved note continuation or
        // trailing matter: preserve it verbatim without manufacturing paragraph/heading roles inside it.
        // Text with no page breaks bounds nothing after the note's body, so the text after it stays readable.
        let limit = if form_feeds.is_empty() { body_end } else { fragment_limit };
        result.exclusions.push(ScalarRange { start: line.scalar_start, end: limit });
        result.note_ranges.push(ScalarRange { start: body_start, end: limit });
        result.blocks.push(block);
    }
    for block in decimal_headings {
        if !result.exclusions.iter().chain(&result.gutter).any(|range|
            range.start < block.range.end && block.range.start < range.end) {
            result.exclusions.push(block.range);
            result.blocks.push(block);
        }
    }
    result
}

pub(super) fn detect_case_headings(evidence: &DocumentInput, text: &ScalarText<'_>, layout: &CaseSourceLayout) -> Vec<Block> {
    // This supplement has CourtListener corpus evidence; other providers retain their readers.
    if evidence.provider != "courtlistener" {
        return Vec::new();
    }
    let nonempty = lines(text).map(|line| case_source_line(text, line))
        .enumerate()
        .filter(|(_, line)| !line.text.trim().is_empty())
        .collect::<Vec<_>>();
    let native_prose = evidence
        .native_claims
        .iter()
        .filter(|claim| claim.kind == EvidenceKind::Prose)
        .map(|claim| claim.range)
        .collect::<HashSet<_>>();
    let notes = evidence
        .native_claims
        .iter()
        .filter(|claim| matches!(claim.kind, EvidenceKind::Footnote | EvidenceKind::Endnote))
        .map(|claim| claim.range)
        .chain(layout.note_ranges.iter().copied())
        .collect::<Vec<_>>();
    let native_headings = evidence
        .native_claims
        .iter()
        .filter(|claim| claim.kind == EvidenceKind::Heading)
        .map(|claim| claim.range)
        .collect::<Vec<_>>();
    let overlaps = |range: ScalarRange, other: &ScalarRange| {
        range.start < other.end && other.start < range.end
    };
    let line_range = |line: Line<'_>| {
        let value = line.text.trim();
        let lead = line.text.len() - line.text.trim_start().len();
        ScalarRange {
            start: text.scalar(line.byte_start + lead),
            end: text.scalar(line.byte_start + lead + value.len()),
        }
    };
    let mut candidates = Vec::<(Block, bool)>::new();
    let mut families = BTreeMap::<(u8, bool, bool), Vec<(usize, u32, usize)>>::new();
    let (mut prose_units, mut sentence_seen) = (0, false);
    for (position, (line_index, line)) in nonempty.iter().enumerate() {
        let value = line.text.trim();
        let range = line_range(*line);
        if evidence.clip_inference(EvidenceKind::Heading, range) != Some(range)
            || notes.iter().any(|note| overlaps(range, note))
            || layout.running_heads.contains(&line.scalar_start)
            // This is a judicial byline; native source heading declarations stay intact.
            || value.trim_end_matches(':').eq_ignore_ascii_case("per curiam")
            || !value.chars().any(char::is_alphabetic)
        {
            continue;
        }
        let words = value.split_whitespace().collect::<Vec<_>>();
        let enumerated = words.first().is_some_and(|word| heading_enumerator(word));
        let bare = words.len() == 1 && enumerated;
        let formal = heading_levels(value).is_some();
        let uppercase = value
            .chars()
            .filter(|character| character.is_alphabetic())
            .all(char::is_uppercase)
            && value
                .chars()
                .filter(|character| character.is_alphabetic())
                .nth(1)
                .is_some();
        let following = nonempty.get(position + 1).map(|(_, line)| line.text.trim());
        // The following numbered paragraph is prose, not its printed paragraph marker.
        let following = following.map(|following| {
            following
                .strip_prefix('[')
                .and_then(|rest| {
                    decimal_prefix(rest, 4).and_then(|(_, length)| rest[length..].strip_prefix(']'))
                })
                .or_else(|| {
                    decimal_prefix(following, 4)
                        .and_then(|(_, length)| following[length..].strip_prefix('.'))
                })
                .map_or(following, str::trim_start)
        });
        let previous = position.checked_sub(1).map(|index| nonempty[index]);
        let separated = previous.is_some_and(|(index, _)| index + 1 < *line_index);
        let centered = nonempty.get(position + 1).is_some_and(|(_, following)| {
            leading_ascii_space(line.text.trim_start_matches('\u{c}'))
                > leading_ascii_space(following.text.trim_start_matches('\u{c}'))
        });
        let form_feed = line.text[..line.text.len() - line.text.trim_start().len()]
            .contains('\u{c}') || line.byte_start.checked_sub(1)
                .is_some_and(|at| text.value.as_bytes()[at] == b'\x0c');
        // This is a layout clue, not a page/paragraph classification of the numeral.
        let preceding_page_line = previous.is_some_and(|(_, previous)| {
            let value = previous.text.trim().trim_matches('-').trim();
            decimal_prefix(value, 4).is_some_and(|(_, length)| length == value.len())
        });
        let source_paragraph = native_prose.contains(&range);
        let heading_context = previous
            .is_some_and(|(_, previous)| !previous.text.trim_end().ends_with(':'))
            && (separated || centered || source_paragraph || form_feed || preceding_page_line)
            && following
                .is_some_and(|following| following.chars().next().is_some_and(char::is_uppercase)
                    && following.split_whitespace().filter(|word|
                        word.chars().next().is_some_and(char::is_lowercase)
                            && word.chars().filter(|character| character.is_alphabetic()).take(3).count() == 3)
                        .take(2).count() == 2);
        // A caption or a title alone does not establish that the reasons have begun.
        // Require prose longer than the heading grammar admits and a completed sentence.
        // PDF soft wraps do not establish a title/sentence-case heading. Keep weaker
        // candidates as gaps until independent source/layout evidence supports them.
        if prose_units > 120 && sentence_seen && formal && (uppercase || bare) && heading_context {
            let index = candidates.len();
            if enumerated {
                let token = words[0];
                let wrapped = token.starts_with('(');
                let inner = token.trim_start_matches('(').trim_end_matches(['.', ')']);
                for (family, value) in enum_readings(inner).into_iter().flatten() {
                    // Bare decimal markers belong to the existing paragraph reader.
                    if family != 0 {
                        if let Ok(value) = value.parse::<u32>() {
                            families
                                .entry((family, wrapped, token.ends_with('.')))
                                .or_default()
                                .push((index, value, position));
                        }
                    }
                }
            }
            candidates.push((
                Block::labelled(NodeKind::Heading, value.to_owned(), range.start, range.end),
                bare,
            ));
        } else if !enumerated
            && !uppercase
            && words
                .iter()
                .filter(|word| word.chars().next().is_some_and(char::is_lowercase))
                .take(2)
                .count()
                == 2
        {
            prose_units += utf16_len(value);
            sentence_seen |= value
                .trim_end_matches(['"', '\'', '\u{201d}', ')', ']'])
                .ends_with(['.', '?', '!']);
        }
    }
    // Use the shared sequence reader so ambiguous C./D. Roman readings cannot interrupt
    // I./II./III. Each supported pair also needs intervening body text and matching style.
    let mut supported = HashSet::new();
    for family in families.values() {
        let markers = family
            .iter()
            .map(|(index, number, _)| Marker {
                number: *number,
                start: candidates[*index].0.range.start,
                content_start: candidates[*index].0.range.end,
                style: MarkerStyle::Bare,
                score: 1.0,
                formal: true,
                sentence: false,
            })
            .collect::<Vec<_>>();
        let positions = family
            .iter()
            .map(|(index, _, position)| (candidates[*index].0.range.start, *position))
            .collect::<HashMap<_, _>>();
        for scope in monotone_scopes(&markers, 1) {
            for pair in scope.windows(2) {
                if positions[&pair[1].start] > positions[&pair[0].start] + 1 {
                    supported.extend([pair[0].start, pair[1].start]);
                }
            }
        }
    }
    candidates
        .into_iter()
        .filter(|(block, bare)| {
            (!bare || supported.contains(&block.range.start))
                && !native_headings
                    .iter()
                    .any(|heading| overlaps(block.range, heading))
                && !layout.blocks.iter().any(|heading|
                    heading.kind == NodeKind::Heading && overlaps(block.range, &heading.range))
        })
        .map(|(block, _)| block)
        .collect()
}

pub(super) fn detect_case_prose(
    evidence: &DocumentInput,
    text: &ScalarText<'_>,
    layout: &CaseSourceLayout,
    recovered: &[Block],
) -> Vec<Block> {
    // Supplement only unaddressed CourtListener text, leaving every supplied or
    // recovered passage intact. Anonymous ranges do not print paragraph numbers.
    if evidence.provider != "courtlistener" {
        return Vec::new();
    }
    let line_range = |line: Line<'_>| {
        let lead = line.text.len() - line.text.trim_start().len();
        ScalarRange { start: text.scalar(line.byte_start + lead), end: text.scalar(line.byte_end) }
    };
    let mut excluded = evidence.exclusions.iter().filter(|item|
        item.applies_to.iter().any(|kind| kind == "paragraph" || kind == "prose"))
        .map(|item| item.range)
        .chain(layout.exclusions.iter().copied())
        .chain(layout.note_ranges.iter().copied())
        .chain(evidence.native_claims.iter().filter(|claim|
            !matches!(claim.kind, EvidenceKind::Section | EvidenceKind::Page)).map(|claim| claim.range))
        .chain(recovered.iter().chain(&layout.blocks).filter(|block|
            !matches!(block.kind, NodeKind::Section | NodeKind::Page)).map(|block| block.range))
        .chain(recovered.iter().chain(&layout.blocks).filter_map(|block| block.marker_range))
        .collect::<Vec<_>>();
    excluded.sort_unstable_by_key(|range| (range.start, range.end));
    let mut merged = Vec::<ScalarRange>::new();
    for range in excluded {
        if range.end <= range.start { continue; }
        if let Some(previous) = merged.last_mut().filter(|previous| range.start <= previous.end) {
            previous.end = previous.end.max(range.end);
        } else { merged.push(range); }
    }
    let mut uncovered = Vec::new();
    let mut cursor = 0;
    for range in &merged {
        if range.start > cursor && text.slice(cursor..range.start)
            .is_some_and(|value| !value.trim().is_empty()) {
            uncovered.push(ScalarRange { start: cursor, end: range.start });
        }
        cursor = cursor.max(range.end);
    }
    if cursor < text.len() && text.slice(cursor..text.len())
        .is_some_and(|value| !value.trim().is_empty()) {
        uncovered.push(ScalarRange { start: cursor, end: text.len() });
    }
    if uncovered.is_empty() { return Vec::new(); }
    // Visit cached lines only in uncovered intervals. Fully represented opinions
    // return above without another whole-document line/word classification pass.
    let cached_lines = text.lines();
    let mut source = Vec::new();
    for range in &uncovered {
        let start = text.byte(range.start);
        let end = text.byte(range.end);
        let mut index = cached_lines.partition_point(|line| line[0] <= start).saturating_sub(1);
        while let Some(line) = cached_lines.get(index).filter(|line| line[0] < end) {
            let mut byte_start = line[0].max(start);
            let byte_end = line[1].min(end);
            // A margin numeral opening the line is the page's gutter: read the line after it,
            // indented to the column its text stands at.
            let mut gutter = 0;
            let first = text.scalar(byte_start + leading_ascii_space(&text.value[byte_start..byte_end]));
            let at = layout.gutter.partition_point(|range| range.end <= first);
            if let Some(range) = layout.gutter.get(at).filter(|range| range.start <= first) {
                gutter = text.byte(range.end).min(byte_end) - byte_start;
                byte_start += gutter;
            }
            if byte_start < byte_end {
                let fragment = case_source_line(text, Line {
                    byte_start, byte_end, scalar_start: text.scalar(byte_start),
                    text: &text.value[byte_start..byte_end],
                });
                if !fragment.text.trim().is_empty() {
                    source.push((index, fragment, gutter + leading_ascii_space(fragment.text)));
                }
            }
            index += 1;
        }
    }
    let blocked = |range: ScalarRange| {
        let at = merged.partition_point(|other| other.end <= range.start);
        merged.get(at).is_some_and(|other| other.start < range.end)
    };
    let mut boundaries = evidence.native_claims.iter().filter(|claim|
        matches!(claim.kind, EvidenceKind::Section | EvidenceKind::Page))
        .flat_map(|claim| [claim.range.start, claim.range.end])
        .chain(recovered.iter().chain(&layout.blocks).filter(|block|
            matches!(block.kind, NodeKind::Section | NodeKind::Page))
            .flat_map(|block| [block.range.start, block.range.end]))
        .chain(merged.iter().flat_map(|range| [range.start, range.end]))
        .chain(text.value.char_indices().filter(|(_, character)| *character == '\u{c}')
            .map(|(byte, _)| text.scalar(byte)))
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    let crosses = |start: usize, end: usize| {
        let at = boundaries.partition_point(|boundary| *boundary < start);
        boundaries.get(at).is_some_and(|boundary| *boundary <= end)
    };
    let lowercase_words = |value: &str| value.split_whitespace().filter(|word|
        word.chars().find(|character| character.is_alphabetic()).is_some_and(char::is_lowercase)
            && word.chars().filter(|character| character.is_alphabetic()).take(3).count() == 3).count();
    let sentence_end = |value: &str| value.trim_end()
        .trim_end_matches(['"', '\'', '\u{201d}', '\u{2019}', ')', ']'])
        .ends_with(['.', '?', '!']);
    let ordinary_line = |line: Line<'_>| !blocked(line_range(line))
        && !layout.running_heads.contains(&line.scalar_start)
        && word_count(line.text, true) >= 8 && lowercase_words(line.text) >= 2;
    let mut indents = source.iter().filter(|(_, line, _)| ordinary_line(*line))
        .map(|(_, _, indent)| *indent).collect::<Vec<_>>();
    let body_indent = median(&mut indents) as usize;
    // A double-spaced PDF has blanks between soft wraps too. Learn that spacing
    // only from adjacent ordinary lines whose first line has not ended a sentence.
    let mut wrap_gaps = source.windows(2).filter_map(|pair| {
        let (index, line, _) = pair[0];
        let (next_index, next, _) = pair[1];
        (ordinary_line(line) && ordinary_line(next) && !sentence_end(line.text)
            && line_range(line).end - line_range(line).start <= 120
            && next_index > index && next_index - index <= 3
            && !crosses(line_range(line).end, line_range(next).start))
            .then(|| next_index - index - 1)
    }).collect::<Vec<_>>();
    let wrap_gap = if wrap_gaps.len() >= 3 { median(&mut wrap_gaps) as usize } else { 0 };
    let mut result = Vec::new();
    let mut emit = |range: ScalarRange| {
        let value = text.slice(range.start..range.end).unwrap_or("");
        let (words, lowercase) = (word_count(value, true), lowercase_words(value));
        // Prose a page break or a note cuts before its sentence ends is still prose.
        if words >= 12 && lowercase >= 4
            && (value.split_whitespace().any(sentence_end) || words >= 20 && lowercase >= 8)
            && evidence.clip_inference(EvidenceKind::Prose, range) == Some(range)
        {
            result.push(Block {
                kind: NodeKind::Prose, range, label: None, aliases: Vec::new(),
                parent_label: None, content_start: None, marker_range: None,
                note: None, diagnostic: None, source: Derivation::Heuristic,
                origin_id: ENGINE_ORIGIN,
            });
        }
    };
    let mut current = None::<ScalarRange>;
    let mut current_body = false;
    let mut previous = None::<(usize, ScalarRange)>;
    for (position, &(index, line, indent)) in source.iter().enumerate() {
        let value = line.text.trim();
        let range = line_range(line);
        if blocked(range) || layout.running_heads.contains(&line.scalar_start)
            || crosses(range.start.saturating_add(1), range.end.saturating_sub(1)) {
            if let Some(range) = current.take() { emit(range); }
            current_body = false;
            previous = None;
            continue;
        }
        let words = value.split_whitespace().collect::<Vec<_>>();
        let enumerated = words.first().is_some_and(|word| heading_enumerator(word));
        let continued_wrap = current_body && !enumerated && indent <= body_indent
            && previous.is_some_and(|(previous_index, previous_range)| {
                let preceding = text.slice(previous_range.start..previous_range.end).unwrap_or("");
                index.saturating_sub(previous_index + 1) <= wrap_gap
                    && previous_range.end - previous_range.start <= 120
                    && !sentence_end(preceding) && !preceding.trim_end().ends_with(':')
                    && !crosses(previous_range.end, range.start)
            });
        // An enumerator opening a line of running prose ("(1) In May 2018, a grand jury indicted
        // the appellant for") opens a paragraph, not a heading.
        let opening = enumerated && ordinary_line(line);
        let title = !opening && !sentence_end(value) && heading_levels(value).is_some()
            && (enumerated || heading_title_case(&words) && !continued_wrap);
        let uppercase = !value.chars().any(char::is_lowercase);
        let columns = value.matches("  ").take(2).count() == 2 && lowercase_words(value) < 3;
        if title || uppercase || columns {
            if let Some(range) = current.take() { emit(range); }
            current_body = false;
            previous = None;
            continue;
        }
        let first_line_indent = indent > body_indent && source.get(position + 1)
            .is_some_and(|(_, _, next)| *next < indent);
        // A numeral enumerator ("(2)", "2.") after a finished sentence starts its paragraph; a lone
        // capital may be an initial ("J. D. Smith"), and a wrapped "(2)" or "43.5." continues one.
        let numeral = words.first().map_or("", |word| word.trim_start_matches('(').trim_end_matches(['.', ')']));
        let numbered = opening && !numeral.is_empty() && numeral.bytes().all(|byte| byte.is_ascii_digit())
            && previous.is_none_or(|(_, previous_range)|
                sentence_end(text.slice(previous_range.start..previous_range.end).unwrap_or("")));
        let separated = numbered || previous.is_some_and(|(previous_index, previous_range)|
            index.saturating_sub(previous_index + 1) > wrap_gap || first_line_indent
                || crosses(previous_range.end, range.start));
        if separated {
            if let Some(range) = current.take() { emit(range); }
            current_body = false;
        }
        current = Some(ScalarRange { start: current.map_or(range.start, |current| current.start), end: range.end });
        current_body |= ordinary_line(line);
        previous = Some((index, range));
    }
    if let Some(range) = current { emit(range); }
    result
}
