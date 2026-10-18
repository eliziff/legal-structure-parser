//! A journal article's structure.

use super::*;

pub(super) fn add_ranges(mut blocks: Vec<Block>, length: usize) -> Vec<Block> {
    for index in 0..blocks.len() {
        blocks[index].range.end = blocks
            .get(index + 1)
            .map_or(length, |value| value.range.start);
    }
    blocks
}

pub(super) fn detect_journal(text: &ScalarText<'_>) -> Vec<Block> {
    let source = javascript_lines(text);
    let section = cached_regex!(
        SECTION,
        r"(?u)^[ \t]*([IVXLCDM]+|[A-Z])\.[ \t]+([^\x08]{3,180})$"
    );
    let mut result = add_ranges(
        source
            .iter()
            .filter_map(|line| section.captures(line.text).map(|capture| (line, capture)))
            .map(|(line, capture)| {
                let whole = capture.get(0).unwrap();
                let lowercase = capture[2].to_lowercase();
                let alias = lowercase
                    .split(|value: char| !value.is_alphanumeric())
                    .filter(|value| !value.is_empty())
                    .fold(
                        String::with_capacity(lowercase.len()),
                        |mut alias, value| {
                            if !alias.is_empty() {
                                alias.push(' ');
                            }
                            alias.push_str(value);
                            alias
                        },
                    );
                let mut block = Block::labelled(
                    NodeKind::Section,
                    format!("sec{}", &capture[1]),
                    text.scalar(line.byte_start + whole.start()),
                    0,
                );
                block.aliases = std::iter::once(capture[1].to_owned())
                    .chain((!alias.is_empty()).then(|| format!("sectitle:{alias}")))
                    .collect();
                block
            })
            .collect(),
        text.len(),
    );
    let note = cached_regex!(NOTE, r"(?u)^[ \t]*(\d{1,5})\t[ \t]*$");
    result.extend(add_ranges(
        source
            .iter()
            .filter_map(|line| note.captures(line.text).map(|capture| (line, capture)))
            .map(|(line, capture)| {
                let whole = capture.get(0).unwrap();
                let mut block = Block::labelled(
                    NodeKind::Footnote,
                    format!("fn{}", capture[1].parse::<u32>().unwrap()),
                    text.scalar(line.byte_start + whole.start()),
                    0,
                );
                block.aliases.push(capture[1].to_owned());
                block
            })
            .collect(),
        text.len(),
    ));
    let mut start = None;
    for (index, line) in source.iter().enumerate() {
        let blank = line.text.trim_matches([' ', '\t', '\r']).is_empty();
        if start.is_none() && !blank {
            start = line
                .text
                .chars()
                .position(|value| !javascript_whitespace(value))
                .map(|at| line.scalar_start + at);
        }
        let next_blank = source
            .get(index + 1)
            .is_some_and(|value| value.text.trim_matches([' ', '\t', '\r']).is_empty());
        if let Some(block_start) = start.filter(|_| next_blank || index + 1 == source.len()) {
            let end = if index + 1 == source.len() {
                text.len()
            } else {
                text.scalar(line.byte_end)
            };
            let value = text
                .slice(block_start..end)
                .expect("journal block range is bounded");
            let first_line = value.split_once('\n').map_or(value, |(line, _)| line);
            let page_marker = first_line
                .find(']')
                .and_then(|end| crate::locator::literal_page_marker(&first_line[..=end], true))
                .is_some_and(|label| !label.is_empty() && label.len() <= 40);
            let block_start = if page_marker {
                value
                    .find('\n')
                    .map_or(end, |at| text.scalar(text.byte(block_start) + at + 1))
            } else {
                block_start
            };
            if block_start < end {
                result.push(Block {
                    kind: NodeKind::Prose,
                    range: ScalarRange {
                        start: block_start,
                        end,
                    },
                    label: None,
                    aliases: Vec::new(),
                    parent_label: None,
                    content_start: None,
                    marker_range: None,
                    note: None,
                    diagnostic: None,
                    source: Derivation::Heuristic,
                    origin_id: ENGINE_ORIGIN,
                });
            }
            start = None;
        }
    }
    result
}
