//! LégisQuébec consolidation pages.

use super::*;

/// A LégisQuébec consolidation page (its HTML) as statute text in the form a provider's statute
/// text takes, for [`crate::statute_outline`]: each heading on a line of its own opened by "#"s for
/// its level ("## PART I HUMAN RIGHTS AND FREEDOMS"), each article and each of its paragraphs on
/// lines of their own ("2. Every human being ..."), without the page's navigation, hidden
/// metadata, history links and amendment notes.
pub fn legisquebec_statute_text(markup: &str) -> String {
    const SKIPPED: [&str; 4] = ["Hidden", "HistoricalNote", "HistoryLink", "modal"];
    struct Open {
        tag: String,
        skipped: bool,
        heading: bool,
        body: bool,
    }
    let (mut text, mut position) = (String::new(), 0usize);
    let mut stack = Vec::<Open>::new();
    let (mut skipping, mut in_body) = (0usize, false);
    // The heading being read: its level (from its label's group) and its words.
    let mut heading: Option<(usize, String)> = None;
    let mut attributes = Attributes::new();
    let mut at = 0;
    while at < markup.len() {
        if markup.as_bytes()[at] != b'<' {
            let end = markup[at..].find('<').map_or(markup.len(), |end| at + end);
            if in_body && skipping == 0 {
                let rendered = normalize_javascript_whitespace(&decode_entities(&markup[at..end]));
                match heading.as_mut() {
                    Some((_, words)) => {
                        if !words.is_empty() && !rendered.is_empty() && !words.ends_with(' ') {
                            words.push(' ');
                        }
                        words.push_str(&rendered);
                    }
                    None => append_text(&mut text, &mut position, &rendered),
                }
            }
            at = end;
            continue;
        }
        if markup[at..].starts_with("<!--") {
            at = markup[at + 4..]
                .find("-->")
                .map_or(markup.len(), |end| at + 4 + end + 3);
            continue;
        }
        let Some(relative_end) = markup[at..].find('>') else {
            at += 1;
            continue;
        };
        let raw = &markup[at..at + relative_end + 1];
        at += relative_end + 1;
        if let Some((tag, _)) = parse_tag(raw, true) {
            let Some(index) = stack.iter().rposition(|open| open.tag == tag) else {
                continue;
            };
            for open in stack.drain(index..).rev() {
                skipping -= usize::from(open.skipped);
                in_body &= !open.body;
                if open.heading {
                    if let Some((level, words)) = heading.take() {
                        let words = normalize_javascript_whitespace(&words);
                        if !words.is_empty() {
                            append_break(&mut text, &mut position);
                            append_text(
                                &mut text,
                                &mut position,
                                &format!("{} {words}", "#".repeat(level + 2)),
                            );
                        }
                    }
                }
            }
            if matches!(tag.as_str(), "div" | "p") && heading.is_none() {
                append_break(&mut text, &mut position);
            }
            continue;
        }
        let Some((tag, attrs)) = parse_tag(raw, false) else {
            continue;
        };
        let closed = attrs.trim_end_matches(javascript_whitespace).ends_with('/');
        parse_attributes(
            attrs
                .trim_end_matches(javascript_whitespace)
                .trim_end_matches('/'),
            &mut attributes,
        );
        let class = attribute(&attributes, "class");
        if tag == "br" && in_body && skipping == 0 && heading.is_none() {
            append_break(&mut text, &mut position);
        }
        if closed || void_tag(&tag) {
            continue;
        }
        let body = !in_body && contains_ascii_word(class, "LegislativeDocument", false);
        in_body |= body;
        let skipped = in_body
            && SKIPPED
                .iter()
                .any(|name| contains_ascii_word(class, name, false));
        skipping += usize::from(skipped);
        // A heading's label names its group ("Label-group4": a chapter); a schedule's is a top one.
        if let Some(group) = class
            .split_whitespace()
            .find_map(|name| name.strip_prefix("Label-group"))
        {
            if let Some((level, _)) = heading.as_mut() {
                *level = group
                    .parse::<usize>()
                    .map_or(0, |group| group.saturating_sub(1).min(3));
            }
        }
        let opens_heading = in_body
            && skipping == 0
            && heading.is_none()
            && (contains_ascii_word(class, "Heading", false)
                || contains_ascii_word(class, "ScheduleHeading", false));
        if opens_heading {
            heading = Some((0, String::new()));
        } else if matches!(tag.as_str(), "div" | "p")
            && in_body
            && skipping == 0
            && heading.is_none()
        {
            append_break(&mut text, &mut position);
        }
        stack.push(Open {
            tag,
            skipped,
            heading: opens_heading,
            body,
        });
    }
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
