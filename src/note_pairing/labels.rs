//! Note labels: a line's opening label token and the labels a document's lines may carry.

use super::*;

pub(super) fn label_token(text: &str, provider: bool) -> Option<LabelToken> {
    let values = chars(text);
    let scalar = ScalarText::new(text);
    let mut cursor = 0;
    while cursor < values.len()
        && cursor < 3
        && (values[cursor].is_whitespace() || "\"'‘’“”.,:;–—-".contains(values[cursor]))
    {
        cursor += 1;
    }
    if provider && text.chars().skip(cursor).take(9).eq("Footnote ".chars()) {
        cursor += 9;
    }
    let pre_start = 0;
    let start = cursor;
    let mut form = "plain";
    let mut token_start = cursor;
    let token_end: usize;
    let mut value = None;
    let mut symbol = String::new();
    // A bracketed label encloses its number as a parenthesized one does.
    let close = match values.get(cursor) {
        Some('(') => Some(')'),
        Some('[') if provider => Some(']'),
        _ => None,
    };
    if let Some(close) = close {
        cursor += 1;
        while values
            .get(cursor)
            .is_some_and(|character| character.is_whitespace())
        {
            cursor += 1;
        }
        token_start = cursor;
        while cursor < values.len() && cursor - token_start < 3 && values[cursor].is_ascii_digit() {
            cursor += 1;
        }
        token_end = cursor;
        while values
            .get(cursor)
            .is_some_and(|character| character.is_whitespace())
        {
            cursor += 1;
        }
        if token_end == token_start || values.get(cursor) != Some(&close) {
            return None;
        }
        cursor += 1;
        value = numeric_value(scalar.slice(token_start..token_end)?);
        form = "paren";
    } else if values
        .get(cursor)
        .is_some_and(|character| is_superscript(*character))
    {
        while cursor < values.len() && cursor - token_start < 3 && is_superscript(values[cursor]) {
            cursor += 1;
        }
        token_end = cursor;
        value = numeric_value(scalar.slice(token_start..token_end)?);
        form = "sup";
    } else if values.get(cursor).is_some_and(char::is_ascii_digit) {
        while cursor < values.len() && cursor - token_start < 3 && values[cursor].is_ascii_digit() {
            cursor += 1;
        }
        token_end = cursor;
        if values.get(cursor).is_some_and(char::is_ascii_digit) {
            return None;
        }
        value = numeric_value(scalar.slice(token_start..token_end)?);
    } else if values
        .get(cursor)
        .is_some_and(|character| is_symbol(*character))
    {
        while cursor < values.len()
            && cursor - token_start < MAX_SYMBOL_RUN
            && is_symbol(values[cursor])
        {
            cursor += 1;
        }
        token_end = cursor;
        symbol = normalize_symbol(scalar.slice(token_start..token_end)?);
        form = "symbol";
    } else {
        return None;
    }
    let end = if form == "paren" { cursor } else { token_end };
    let post_start = cursor;
    while cursor < values.len() && cursor - post_start < 2 && ".)],".contains(values[cursor]) {
        cursor += 1;
    }
    let observed = scalar.slice(if form == "paren" { start } else { token_start }..end)?;
    let embedded_endnote = format!("endnote {observed}");
    let remainder = scalar.slice(cursor..scalar.len())?;
    let match_end = if form == "plain"
        && remainder
            .get(..embedded_endnote.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&embedded_endnote))
    {
        cursor + embedded_endnote.chars().count()
    } else {
        cursor
    };
    Some(LabelToken {
        pre_start,
        start: if form == "paren" { start } else { token_start },
        end,
        match_end,
        observed: observed.to_owned(),
        value,
        symbol,
        form,
        post: scalar.slice(post_start..cursor)?.to_owned(),
    })
}

pub(super) fn heading_shaped(body: &str) -> bool {
    let mut letters = 0;
    body.trim().chars().take(40).all(|character| {
        if character.is_alphabetic() {
            letters += 1;
            character.is_uppercase()
        } else {
            true
        }
    }) && letters >= 4
}

pub(super) fn paren_style(lines: &[PairLine<'_>]) -> bool {
    let mut paren = 0;
    let mut plain = 0;
    for line in lines.iter().filter(|line| line.zone == Zone::Note) {
        let Some(token) = label_token(&line.text, provider_text(line)) else {
            continue;
        };
        if token.form == "paren" || (token.form == "plain" && token.post.contains(')')) {
            paren += 1;
        } else if matches!(token.form, "plain" | "sup") {
            plain += 1;
        }
    }
    paren >= 3 && paren > 2 * plain
}

pub(super) fn spaced_symbol_separator(text: &str) -> bool {
    let mut parts = 0;
    text.split_whitespace().all(|part| {
        parts += 1;
        part.chars().count() == 1 && part.chars().all(is_symbol)
    }) && parts >= 3
}

pub(super) fn day_list_date(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*\d{1,2}(?:\s*,\s*\d{1,2})+\s+(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\b",
        )
        .unwrap()
    })
    .is_match(text)
}

pub(super) fn month_year_start(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(?i)^\s*{MONTH}\.?,?\s+\d{{4}}\b")).unwrap())
        .is_match(text)
}

pub(super) fn volume_cite_start(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let Some(found) = RE
        .get_or_init(|| {
        Regex::new(
            r"^\d{1,3}\s+[A-Z][A-Za-z]{0,5}\.(?:(?:\s+(?:&\s*)?|&\s*)[A-Z][A-Za-z]{0,5}\.?|[A-Z]\.)*\s*(?:\(\d{1,4}[a-z]{0,2}\)\s*)?(?:(?:c|ch|ss?)\.\s*)?\d{1,4}",
        )
        .unwrap()
    })
        .find(text)
    else {
        return false;
    };
    text[found.end()..]
        .chars()
        .next()
        .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
}

pub(super) fn bracket_year_body(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[(?:1[5-9]|20)\d\d\]").unwrap())
        .is_match(text)
}

/// Whether words stand on the line's own row, on either side: a paragraph's or a form
/// item's text set beside its number, or a form's line around a blank's number. A note's
/// label alone on its line has its text below.
pub(super) fn words_on_row(lines: &[PairLine<'_>], index: usize) -> bool {
    let line = &lines[index];
    let height = line.height();
    let same_page = |other: &&PairLine<'_>| other.page == line.page;
    let start = lines[..index].iter().rev().take_while(same_page).count();
    let end = lines[index..].iter().take_while(same_page).count();
    height > 0.0
        && lines[index - start..index + end].iter().any(|other| {
            let overlap = line.bbox[3].min(other.bbox[3]) - line.bbox[1].max(other.bbox[1]);
            other.idx != line.idx
                && overlap > 0.5 * height.min(other.height())
                && other.text.chars().any(char::is_alphabetic)
        })
}

pub(super) fn extract_labels(lines: &[PairLine<'_>], refs: &[Candidate], first_page: u32) -> Vec<Candidate> {
    let mut ref_pages: HashMap<u32, HashSet<u32>> = HashMap::new();
    for candidate in refs
        .iter()
        .filter(|candidate| !candidate.repaired && !candidate.flags.paren_ref)
    {
        if let Some(value) = candidate.value {
            ref_pages
                .entry(value)
                .or_default()
                .insert(lines[candidate.line].page);
        }
    }
    let paren_style = paren_style(lines);
    let mut candidates = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        if line.suppress_footnote_label || line.edge_folio {
            continue;
        }
        let Some(token) = label_token(&line.text, provider_text(line)) else {
            continue;
        };
        let paired_note_region = (line.zone == Zone::Note && !line.region_witness_demoted
            || line.note_column_fit)
            && token.value.is_some_and(|value| {
                ref_pages.get(&value).is_some_and(|pages| {
                    pages.iter().any(|page| {
                        line.page.abs_diff(*page) <= 1 || line.note_region_mode == "endnote"
                    })
                })
            });
        // A note such as "1 S.C. 1997, c. 12" can also parse as a reporter
        // citation beginning with volume 1. Independent reference and note-region
        // evidence keep both readings alive for the sequence assignment.
        if line.protected(token.start, token.end) && !paired_note_region {
            continue;
        }
        let scalar = ScalarText::new(&line.text);
        let body = scalar
            .slice(token.match_end..scalar.len())
            .expect("token offsets must be valid");
        let follow = body.chars().next();
        if token.form == "plain"
            && token.value.is_some()
            && day_list_date(
                scalar
                    .slice(token.start..scalar.len())
                    .expect("token offsets must be valid"),
            )
        {
            continue;
        }
        if token.post.is_empty()
            && follow.is_some_and(|value| {
                !value.is_whitespace()
                    && !value.is_uppercase()
                    && !QUOTES.contains(value)
                    && !(value == '[' && bracket_year_body(&body))
            })
        {
            continue;
        }
        if token.symbol.is_empty() && token.value.is_none() {
            continue;
        }
        // A decimal ("17.93"), a zero-padded number ("05") or a date ("13 May 2026")
        // opens the line, not a note's label.
        if token.value.is_some()
            && ((token.observed.len() > 1 && token.observed.starts_with('0'))
                || (token.post.starts_with('.')
                    && follow.is_some_and(|value| value.is_ascii_digit()))
                || (token.form == "plain" && month_year_start(&body)))
        {
            continue;
        }
        if token.form == "plain" && follow.is_some_and(|value| value.is_lowercase()) {
            continue;
        }
        if token.form == "symbol" {
            if spaced_symbol_separator(&line.text) {
                continue;
            }
            let bare = body.trim().is_empty();
            if !(bare && line.page - first_page < SYMBOL_START_PAGE_LIMIT)
                && body.trim().chars().count() < 4
            {
                continue;
            }
        }
        let body_stripped = body.trim();
        if token.value.is_some() && line.zone != Zone::Note && heading_shaped(body_stripped) {
            continue;
        }
        // A number and its period alone on a body line ("4.") is a numbered paragraph's
        // number when the layout set its words beside it on the same row: never a note's
        // label. So is a bare number at the body's size ("22"), as Singapore's courts print
        // theirs. A note's label alone on its line, its text below, stays one.
        if token.value.is_some()
            && line.zone == Zone::Body
            && body_stripped.is_empty()
            && (token.post.trim() == "." || (token.post.is_empty() && !line.small_font))
            && words_on_row(lines, line_index)
        {
            continue;
        }
        // So is a number and its period opening a body line in the body's own type, its words
        // running on beside it ("1. SERVICE: Once you have filed"): a note's label sits in the
        // notes' region or in their smaller type. A line that carries no type, as recognition
        // reads it, says neither.
        if token.value.is_some()
            && line.zone == Zone::Body
            && !line.spans.is_empty()
            && !line.small_font
            && token.post.trim() == "."
            && !body_stripped.is_empty()
        {
            continue;
        }
        // So is a bare or bracketed number opening a body line in the body's type with prose
        // beside it ("4  Where a compromise is proposed", "(8)  Any claim by"): a statute's
        // section or subsection, or a judgment's paragraph. A note set in the body's type opens
        // as a citation ("9 [1991] 2 S.C.R. 577", "6 Ibid., p. 8"), not prose citing one later.
        if token.value.is_some()
            && matches!(token.form, "plain" | "paren")
            && line.zone == Zone::Body
            && !line.spans.is_empty()
            && !line.small_font
            && !body_stripped.is_empty()
            && !cues::layout_has_citation_cue(body_stripped.split_whitespace().next().unwrap_or(""))
        {
            continue;
        }
        if token.value.is_some()
            && token.post.contains(',')
            && line.zone != Zone::Note
            && body_stripped
                .chars()
                .next()
                .is_some_and(|character| character.is_lowercase())
        {
            continue;
        }
        let mut score = match line.zone {
            Zone::Note => 3.0,
            Zone::Body => 0.2,
            Zone::Title => -2.0,
            Zone::Header | Zone::Number => -1.2,
            Zone::Visual => -1.5,
        };
        if line.zone == Zone::Note && line.region_witness_demoted {
            score = 0.6;
        }
        if matches!(line.zone, Zone::Header | Zone::Number)
            && !line.prose_like
            && body_stripped.is_empty()
        {
            continue;
        }
        if token.form == "sup" {
            score += 1.0;
        } else if token.form == "paren" || (token.form == "plain" && token.post.contains(')')) {
            score += if paren_style { 0.4 } else { -1.8 };
        } else if !token.post.is_empty() || follow.is_some_and(char::is_whitespace) {
            score += 0.8;
        }
        if !body_stripped.is_empty() {
            if cues::layout_has_citation_cue(body_stripped) {
                score += 0.7;
            } else if body_stripped.chars().count() >= 8 {
                score += 0.4;
            }
            if cues::layout_is_citation_continuation(body_stripped) {
                score -= 1.2;
            }
        } else if !(token.form == "symbol" && line.page - first_page < SYMBOL_START_PAGE_LIMIT) {
            score -= 0.9;
        }
        if line.note_column_fit {
            score += 0.7;
        }
        if line.small_font {
            score += 0.4;
        }
        let junk_prefix = scalar
            .slice(token.pre_start..token.start)
            .expect("token offsets must be valid")
            .trim()
            .to_owned();
        if !junk_prefix.is_empty() {
            score -= 0.6;
        }
        let ref_supported = token.value.is_some_and(|value| {
            ref_pages
                .get(&value)
                .is_some_and(|pages| pages.iter().any(|page| line.page.abs_diff(*page) <= 1))
        });
        if ref_supported {
            score += 0.9;
        }
        // Paragraph layout is a competing witness, not a veto before pairing.
        if line.paragraph_label_candidate {
            score -= 0.9;
        }
        let candidate = Candidate {
            line: line_index,
            start: token.start,
            end: token.end,
            observed: token.observed.clone(),
            value: token.value,
            symbol: token.symbol,
            form: token.form,
            score,
            reason: "line_start_label",
            repaired: !junk_prefix.is_empty(),
            repair_kind: if junk_prefix.is_empty() {
                ""
            } else {
                "junk_prefix_stripped"
            },
            requires_visual_cue: false,
            flags: CandidateFlags {
                ref_supported,
                ..CandidateFlags::default()
            },
        };
        candidates.push(candidate.clone());
        if token.form == "plain" && token.post.is_empty() && token.observed.chars().count() >= 2 {
            let observed = ScalarText::new(&token.observed);
            for cut in 1..observed.len() {
                let head = observed
                    .slice(0..cut)
                    .expect("candidate offsets must be valid");
                let rest = format!(
                    "{}{}",
                    observed
                        .slice(cut..observed.len())
                        .expect("candidate offsets must be valid"),
                    body
                );
                let Some(head_value) = numeric_value(&head) else {
                    continue;
                };
                if !volume_cite_start(&rest) {
                    continue;
                }
                let mut split = candidate.clone();
                split.end = split.start + cut;
                split.observed = head.to_owned();
                split.value = Some(head_value);
                split.score -= 1.1;
                split.reason = "glued_volume_split";
                split.flags.volume_split = true;
                candidates.push(split);
                break;
            }
        }
    }
    let mut by_value: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if !candidate.flags.volume_split {
            if let Some(value) = candidate.value {
                by_value.entry(value).or_default().push(index);
            }
        }
    }
    for indexes in by_value.values().filter(|indexes| indexes.len() >= 2) {
        for &index in indexes {
            let candidate = &candidates[index];
            let line = ScalarText::new(&lines[candidate.line].text);
            let continuation = line
                .slice(candidate.start..line.len())
                .expect("candidate offsets must be valid");
            if volume_cite_start(&continuation)
                && indexes.iter().any(|other| {
                    *other != index
                        && lines[candidates[*other].line]
                            .idx
                            .abs_diff(lines[candidate.line].idx)
                            <= 3
                })
            {
                candidates[index].score -= 1.2;
            }
        }
    }
    candidates
}
