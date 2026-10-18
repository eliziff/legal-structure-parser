//! Printed and physical pages.

use super::*;

pub(super) struct PageMarker {
    number: u32,
    start: usize,
    content_start: usize,
}

pub(super) fn page_markers(text: &ScalarText<'_>, report_start: Option<u32>) -> Vec<PageMarker> {
    let page_word = cached_regex!(PAGE_WORD, r"(?iu)\bpage\b");
    if !page_word.is_match(text.value) {
        return Vec::new();
    }
    let regex = cached_regex!(
        VALUE,
        r"(?imu)\[[ \t]*pages?[ \t]*[.:,;]?[ \t]*(\d{1,4})[ \t]*[.:,;]?[ \t]*[\]\[)}]?[ \t]*[.,;:]?|^[ \t]*\[?[ \t]*page[ \t]*[.:,;]?[ \t]*(\d{1,4})[ \t]*[\])}]?[ \t]*[.,;:]?[ \t]*$"
    );
    let mut result = Vec::new();
    for line in lines(text).filter(|line| {
        line.text
            .as_bytes()
            .windows(4)
            .any(|word| word.eq_ignore_ascii_case(b"page"))
    }) {
        for capture in regex.captures_iter(line.text) {
            let whole = capture.get(0).unwrap();
            let number = capture
                .get(1)
                .or_else(|| capture.get(2))
                .unwrap()
                .as_str()
                .parse::<u32>()
                .unwrap();
            if report_start.is_some_and(|start| number < start) {
                continue;
            }
            let start = text.scalar(line.byte_start + whole.start());
            let content_start = text.scalar(line.byte_start + whole.end());
            result.push(PageMarker {
                number,
                start,
                content_start,
            });
        }
    }
    result
}

pub(super) fn detect_pages(
    text: &ScalarText<'_>,
    report_start: Option<u32>,
    require_report_start: bool,
) -> Vec<Block> {
    if require_report_start && report_start.is_none() {
        return Vec::new();
    }
    let mut scopes = Vec::<Vec<PageMarker>>::new();
    let mut by_last = HashMap::<u32, Vec<usize>>::new();
    for marker in page_markers(text, report_start) {
        let number = marker.number;
        let index = marker
            .number
            .checked_sub(1)
            .and_then(|number| by_last.get(&number))
            .into_iter()
            .flatten()
            .copied()
            .reduce(|best, current| {
                if scopes[current].last().unwrap().start > scopes[best].last().unwrap().start {
                    current
                } else {
                    best
                }
            })
            .unwrap_or(scopes.len());
        if index == scopes.len() {
            scopes.push(vec![marker]);
        } else {
            let previous = scopes[index].last().unwrap().number;
            if let Some(values) = by_last.get_mut(&previous) {
                values.retain(|value| *value != index);
            }
            scopes[index].push(marker);
        }
        by_last.entry(number).or_default().push(index);
    }
    let mut best = Vec::new();
    let mut tied = false;
    for scope in scopes.into_iter().filter(|scope| scope.len() >= 3) {
        match scope.len().cmp(&best.len()) {
            std::cmp::Ordering::Greater => {
                best = scope;
                tied = false;
            }
            std::cmp::Ordering::Equal => tied = true,
            std::cmp::Ordering::Less => {}
        }
    }
    if best.is_empty() || tied {
        return Vec::new();
    }
    let mut blocks = best
        .windows(2)
        .map(|pair| {
            Block::labelled(
                NodeKind::Page,
                format!("page{}", pair[0].number),
                pair[0].content_start,
                pair[1].start,
            )
        })
        .collect::<Vec<_>>();
    if report_start.is_some_and(|start| best[0].number == start + 1) {
        blocks.insert(
            0,
            Block::labelled(
                NodeKind::Page,
                format!("page{}", report_start.unwrap()),
                0,
                best[0].start,
            ),
        );
    }
    blocks
}

/// Form feeds are the source's own page breaks, so a physical page no printed number marks is
/// still a page. It takes the number the printed pages count it to, else its place in the text.
pub(super) fn form_feed_pages(evidence: &DocumentInput, text: &ScalarText<'_>, blocks: &[Block]) -> Vec<Block> {
    let form_feeds = text.value.char_indices().filter(|(_, character)| *character == '\u{c}')
        .map(|(byte, _)| text.scalar(byte)).collect::<Vec<_>>();
    if form_feeds.is_empty() {
        return Vec::new();
    }
    let pages = blocks.iter().filter(|block| block.kind == NodeKind::Page)
        .map(|block| (block.range, block.label.as_deref(), block.marker_range))
        .chain(evidence.native_claims.iter().filter(|claim| claim.kind == EvidenceKind::Page)
            .map(|claim| (claim.range, claim.label.as_deref(), None)))
        .collect::<Vec<_>>();
    let mut offsets = HashMap::<i64, usize>::new();
    for (_, label, marker) in &pages {
        let (Some(label), Some(marker)) = (label, marker) else { continue; };
        let Ok(number) = label.trim_start_matches("page").parse::<i64>() else { continue; };
        let ordinal = form_feeds.partition_point(|boundary| *boundary < marker.start) as i64 + 1;
        *offsets.entry(number - ordinal).or_default() += 1;
    }
    let offset = offsets.into_iter().max_by_key(|&(offset, count)| (count, -offset.abs()))
        .map_or(0, |(offset, _)| offset);
    let labels = pages.iter().filter_map(|(_, label, _)| *label).collect::<HashSet<_>>();
    let mut result = Vec::new();
    let mut start = 0;
    for (index, end) in form_feeds.iter().copied().chain([text.len()]).enumerate() {
        let range = ScalarRange { start, end };
        start = end + 1;
        let number = index as i64 + 1 + offset;
        let label = number.to_string();
        if number < 1 || labels.contains(label.as_str())
            || text.slice(range.start..range.end).is_none_or(|value| value.trim().is_empty())
            || pages.iter().any(|(page, _, _)| page.start < range.end && range.start < page.end) {
            continue;
        }
        let mut block = Block::labelled(NodeKind::Page, label.clone(), range.start, range.end);
        block.aliases.push(label);
        result.push(block);
    }
    result
}
