//! Numbered paragraphs: their markers, the counts they form, and the ranges each count gives.

use super::*;

pub(super) fn case_source_line<'a>(text: &ScalarText<'_>, mut line: Line<'a>) -> Line<'a> {
    let prefix = line.text.len() - line.text.trim_start_matches([' ', '\t', '\u{c}']).len();
    if let Some(at) = line.text[..prefix].rfind('\u{c}') {
        let offset = at + 1;
        line.byte_start += offset;
        line.scalar_start = text.scalar(line.byte_start);
        line.text = &line.text[offset..];
    }
    line
}

pub(super) fn paragraph_markers(text: &ScalarText<'_>, contiguous: bool, courtlistener: bool) -> Vec<Marker> {
    let mut result = Vec::new();
    for line in lines(text) {
        let line = if courtlistener { case_source_line(text, line) } else { line };
        let lead = leading_ascii_space(line.text);
        let value = &line.text[lead..];
        // A page marker ("*517 ", "*Page 516 ") or a caption in capitals ("JOURNAL ENTRY AND OPINION ") can
        // share its line with the first bracketed paragraph number; that paragraph opens at the bracket.
        let shift = if courtlistener && !value.starts_with(['[', '{']) {
            value.find("{¶").or_else(|| value.find("[¶")).filter(|&at| at > 0 && at <= 80 && {
                // Each word before it a star page ("*517", "*Page 4") or capitals ("I", "A", "JOURNAL ENTRY").
                let prefix = value[..at].trim_end();
                let words = prefix.split_ascii_whitespace().collect::<Vec<_>>();
                let digits = |word: &str| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit());
                let mut index = 0;
                while index < words.len() {
                    let word = words[index];
                    if word == "*Page" && words.get(index + 1).is_some_and(|next| digits(next)) {
                        index += 2;
                    } else if word.strip_prefix('*').is_some_and(digits) || (word.bytes().any(|byte| byte.is_ascii_uppercase())
                        && !word.bytes().any(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())) {
                        index += 1;
                    } else {
                        break;
                    }
                }
                prefix.is_ascii() && !words.is_empty() && index == words.len()
            }).unwrap_or(0)
        } else { 0 };
        let value = &value[shift..];
        let start = if shift > 0 { line.scalar_start + lead + shift } else { line.scalar_start };
        // Public-domain citation courts bracket the number, mostly with a pilcrow: "[¶ 12]",
        // South Dakota's "[¶ 12.]", Ohio's "{¶ 12}", New Mexico's "{12}".
        let pilcrow = courtlistener.then(|| {
            let close = match value.as_bytes().first() { Some(b'[') => ']', Some(b'{') => '}', _ => return None };
            let inner = value[1..].trim_start_matches([' ', '\t']);
            let inner = inner.strip_prefix('¶').or((close == '}').then_some(inner))?;
            let rest = inner.trim_start_matches([' ', '\t']);
            let (number, length) = decimal_prefix(rest, 4)?;
            let after = rest[length..].strip_prefix('.').unwrap_or(&rest[length..]).trim_start_matches([' ', '\t']);
            after.strip_prefix(close)?;
            Some((number, MarkerStyle::Bracket, value.len() - after.len() + 1))
        }).flatten();
        let basic = if pilcrow.is_some() {
            pilcrow
        } else if let Some(rest) = value.strip_prefix('[') {
            decimal_prefix(rest, 4).and_then(|(number, length)| {
                (rest.as_bytes().get(length) == Some(&b']'))
                    .then(|| (number, MarkerStyle::Bracket, length + 2))
            })
        } else {
            decimal_prefix(value, 4).and_then(|(number, length)| {
                let rest = &value[length..];
                if rest.starts_with('.')
                    && (rest[1..].chars().next().is_some_and(char::is_whitespace)
                        || (rest.len() == 1 && line.byte_end < text.value.len()))
                {
                    Some((number, MarkerStyle::Dot, length + 1))
                } else if contiguous
                    && rest.starts_with('.')
                    && rest[1..].chars().next().is_some_and(char::is_uppercase)
                {
                    Some((number, MarkerStyle::Dot, length + 1))
                } else if rest.chars().next().is_some_and(char::is_whitespace)
                    || (rest.is_empty() && line.byte_end < text.value.len())
                {
                    Some((
                        number,
                        if contiguous {
                            MarkerStyle::Dot
                        } else {
                            MarkerStyle::Bare
                        },
                        length,
                    ))
                } else {
                    None
                }
            })
        };
        if let Some((number, style, marker_end)) = basic {
            let content = marker_end + leading_ascii_space(&value[marker_end..]);
            result.push(Marker {
                number: number.parse().unwrap(),
                start,
                content_start: line.scalar_start + lead + shift + content,
                style,
                score: 1.0,
                formal: false,
                sentence: false,
            });
        }
        {
            // Oklahoma's OSCN serves its pilcrow as mojibake, "Â¶1".
            let value = if courtlistener {
                value.strip_prefix('Â').filter(|rest| rest.starts_with('¶')).unwrap_or(value)
            } else {
                value
            };
            let glyph = value
                .chars()
                .next()
                .filter(|value| matches!(value, '¶' | '\u{95}' | '•'));
            if let Some(glyph) = glyph {
                let rest = value[glyph.len_utf8()..].trim_start_matches([' ', '\t']);
                if let Some((number, length)) = decimal_prefix(rest, 4) {
                    let after = rest[length..].chars().next();
                    if after.is_none_or(|value| value.is_whitespace() || ".,;:—-".contains(value))
                    {
                        result.push(Marker {
                            number: number.parse().unwrap(),
                            start,
                            content_start: text
                                .scalar(line.byte_start + line.text.len() - rest.len() + length),
                            style: MarkerStyle::Dot,
                            score: 1.0,
                            formal: false,
                            sentence: false,
                        });
                    }
                }
            }
        }
    }
    result
}

pub(super) fn marker_visible(marker: &Marker, excluded: &[ScalarRange]) -> bool {
    !excluded
        .iter()
        .any(|range| range.start <= marker.start && marker.start < range.end)
}

pub(super) fn word_count(value: &str, letters_only: bool) -> usize {
    let mut count = 0;
    let mut inside = false;
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        let member = if letters_only {
            character.is_alphabetic()
        } else {
            character.is_alphanumeric()
        };
        if member {
            count += usize::from(!inside);
            inside = true;
        } else if matches!(character, '\'' | '’')
            && inside
            && characters.peek().is_some_and(|next| {
                if letters_only {
                    next.is_alphabetic()
                } else {
                    next.is_alphanumeric()
                }
            })
        {
            continue;
        } else {
            inside = false;
        }
    }
    count
}

pub(super) fn median(values: &mut [usize]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle] as f64
    } else {
        (values[middle - 1] + values[middle]) as f64 / 2.0
    }
}

pub(super) fn heading_joined(text: &ScalarText<'_>, known: [Option<&HashSet<usize>>; 2]) -> [Vec<Marker>; 2] {
    let mut result = std::array::from_fn(|_| Vec::new());
    for line in lines(text) {
        let bytes = line.text.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            let (style, index, digits) = match bytes[at] {
                b'[' if known[0].is_some() => (MarkerStyle::Bracket, 0, at + 1),
                byte if byte.is_ascii_digit() && known[1].is_some() => (MarkerStyle::Dot, 1, at),
                _ => {
                    at += 1;
                    continue;
                }
            };
            let length = bytes[digits..]
                .iter()
                .take(4)
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if length == 0 {
                at += 1;
                continue;
            }
            let tail = digits + length;
            let end = if style == MarkerStyle::Bracket {
                (bytes.get(tail) == Some(&b']')).then_some(tail + 1)
            } else if bytes.get(tail) == Some(&b'.') {
                let after = tail + 1;
                if after == bytes.len() {
                    Some(after)
                } else {
                    line.text[after..]
                        .chars()
                        .next()
                        .filter(|character| character.is_whitespace())
                        .map(|character| after + character.len_utf8())
                }
            } else {
                None
            };
            let Some(end) = end else {
                at += 1;
                continue;
            };
            let start = text.scalar(line.byte_start + at);
            if known[index].is_some_and(|known| known.contains(&start)) {
                at = end;
                continue;
            }
            let heading = &line.text[..at];
            let formal = formal_heading(heading)
                && (style == MarkerStyle::Bracket || !heading.contains('.'));
            let sentence = style == MarkerStyle::Bracket
                && sentence_heading(heading, &text.value[line.byte_start + end..]);
            if formal || sentence {
                result[index].push(Marker {
                    number: line.text[digits..tail].parse().unwrap(),
                    start,
                    content_start: text.scalar(line.byte_start + end),
                    style,
                    score: if formal { 0.6 } else { 0.35 },
                    formal,
                    sentence,
                });
            }
            at = end;
        }
    }
    result
}

pub(super) fn rooted_chain(candidates: &[Marker]) -> (Vec<Marker>, f64) {
    let selected = select_numeric_sequence(
        candidates
            .iter()
            .enumerate()
            .map(|(index, marker)| NumericSequenceCandidate {
                index,
                value: marker.number,
                position: (marker.start, 0),
                page: 0,
                score: marker.score,
                start_supported: false,
            })
            .collect(),
        NumericSequencePolicy::RootedConsecutive,
    );
    (
        selected
            .indices
            .into_iter()
            .map(|index| candidates[index].clone())
            .collect(),
        selected.score,
    )
}

/// A rooted count's paragraphs whose numbers a provider's flattened text runs into the line before them, a heading
/// the heading grammar does not read as one ("the background facts The Primary Will [9] In the Primary Will …",
/// "… by the end of 2018? [34] The Claimant …", "… Decision Under Review.[8]The Board …") or misprints ("{3] At
/// the hearing"): a bracketed number at a line's start or after a space or a heading's closing punctuation, and
/// before a capital or a quotation mark, spaced or not. The count is
/// read again with them, and where it runs longer the labels it takes join the candidates; the rest are dropped, so
/// a year's "[2003] OJ" never reaches any other reading.
pub(super) fn bridge_glued_labels(text: &ScalarText<'_>, candidates: &mut Vec<Marker>, chain: &mut Vec<Marker>, score: &mut f64) {
    let held = candidates.iter().map(|value| value.start).collect::<HashSet<_>>();
    // A note's call is never the count's next number where it stands, so endnotes need no pairing here: a decision
    // numbering its notes as its paragraphs ("I. Introduction [1] The appellant …" beside "[1] 2012 CM 3009.") would
    // lose its paragraphs to a pairing by number alone.
    let mut glued = Vec::new();
    for line in lines(text) {
        let mut from = 0;
        while let Some(offset) = line.text[from..].find(['[', '{']) {
            let at = from + offset;
            from = at + 1;
            // At the line's start or after a space or the punctuation that ends a heading ("Decision Under
            // Review.[8]"), never after a word, as a note's call runs into it ("Ontario[1]").
            if at > 0 && !line.text[..at].ends_with(|value: char| value.is_whitespace()
                || ['.', '?', '!', ':', ';', '"', '\u{201d}', '\u{2019}', ')', ']'].contains(&value))
            {
                continue;
            }
            // A brace typed for a bracket ("{3] At the hearing") is the same number.
            let digits = &line.text[at + 1..];
            let length = digits.bytes().take(4).take_while(u8::is_ascii_digit).count();
            if length == 0 || !matches!(digits.as_bytes().get(length), Some(b']' | b'}')) {
                continue;
            }
            let end = at + length + 2;
            let following = line.text[end..].trim_start_matches([' ', '\t']);
            let number = digits[..length].parse::<u32>().unwrap();
            let start = text.scalar(line.byte_start + at);
            if !following.chars().next().is_some_and(|value| value.is_uppercase()
                || ['"', '\u{201c}', '\u{2018}', '\'', '\u{ab}'].contains(&value))
            {
                continue;
            }
            if !held.contains(&start) {
                glued.push(Marker {
                    number,
                    start,
                    content_start: text.scalar(line.byte_start + end),
                    style: MarkerStyle::Bracket,
                    score: 0.2,
                    formal: false,
                    sentence: false,
                });
            }
        }
    }
    if glued.is_empty() {
        return;
    }
    let pool = candidates.iter().chain(&glued).cloned().collect::<Vec<_>>();
    let (longer, longer_score) = rooted_chain(&pool);
    if longer.len() <= chain.len() {
        return;
    }
    let used = longer.iter().map(|value| value.start).collect::<HashSet<_>>();
    candidates.extend(glued.into_iter().filter(|value| used.contains(&value.start)));
    *chain = longer;
    *score = longer_score;
}

pub(super) fn sole_chain(chain: &[Marker], candidates: &[Marker]) -> bool {
    let claimed = chain
        .iter()
        .map(|value| value.start)
        .collect::<HashSet<_>>();
    let last = chain.last().map_or(0, |value| value.number);
    let mut rest = candidates
        .iter()
        .filter(|value| !claimed.contains(&value.start))
        .collect::<Vec<_>>();
    rest.sort_by_key(|value| value.start);
    !rest
        .iter()
        .any(|value| (1..=last + 1).contains(&value.number))
        && rest.windows(2).all(|pair| pair[1].number <= pair[0].number)
}

/// Where a second count from 1 opens after a first of at least three and runs through at least half
/// the numbers after it, as a note list does: "[1]" to "[262]" after paragraphs "[1]" to "[260]". A
/// stray "1" in the front matter before the paragraphs' own count opens no first count.
pub(super) fn note_list_start(candidates: &[Marker]) -> Option<usize> {
    let mut sorted = candidates.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|marker| marker.start);
    let run = |markers: &[&Marker]| markers.iter().fold(1, |expected, later| if later.number == expected { expected + 1 } else { expected }) as usize - 1;
    sorted.iter().enumerate().skip(1).rev().filter(|(_, marker)| marker.number == 1).find_map(|(at, marker)| {
        let (before, after) = sorted.split_at(at);
        let count = run(after);
        (count >= 3 && count * 2 >= after.len() && run(before) >= 3).then_some(marker.start)
    })
}

/// A count whose numbers sit in a text's last quarter, opening past its middle: a note list. A count
/// opening earlier is the decision's paragraphs, however many short ones close it ("[6] HUDDART,
/// J.A.: I agree.").
pub(super) fn endnote_shaped(text: &ScalarText<'_>, chain: &[Marker]) -> bool {
    let length = text.utf16_len();
    chain.len() >= 8
        && length > 0
        && text.utf16(chain[0].start) as f64 > length as f64 * 0.5
        && chain
            .iter()
            .filter(|value| text.utf16(value.start) as f64 > length as f64 * 0.75)
            .count() as f64
            / chain.len() as f64
            >= 0.7
}

/// Where another count from 1 opens among a rooted count's other numbers: "[1]" followed by "[2]"
/// and "[3]", another writing's paragraphs or a note list. A paragraph's quoted numbers ("[42] This
/// matters." under [76]) open none.
pub(super) fn restart_boundaries(candidates: &[Marker], chain: &[Marker]) -> Vec<Marker> {
    let held = chain.iter().map(|marker| marker.start).collect::<HashSet<_>>();
    let mut rest = candidates.iter().filter(|marker| !held.contains(&marker.start)).collect::<Vec<_>>();
    rest.sort_by_key(|marker| marker.start);
    rest.iter().enumerate().filter(|(index, marker)| {
        marker.number == 1 && rest[index + 1..].iter().fold(2, |expected, later| {
            if later.number == expected { expected + 1 } else { expected }
        }) > 3
    }).map(|(_, marker)| (*marker).clone()).collect()
}

/// A judge's or member's signature, or a release line, as a decision prints them after its reasons
/// ("“K. Feldman J.A.”", "“I agree. L.B. Roberts J.A.”", "Released: November 28, 2022").
pub(super) fn signature_line(value: &str) -> bool {
    let value = value.trim().trim_matches(['"', '“', '”', '\'', '‘', '’']).trim();
    !value.is_empty()
        && value.chars().count() <= 90
        && (cached_regex!(
            SIGNATURE,
            r"\b(?:J|JJ|C\.?J|C\.?J\.?C|C\.?J\.?O|J\.?A|JJ\.?A|J\.?C\.?A|Q\.?C|K\.?C)\.(?:\s*\((?:ad hoc|dissenting|concurring)\))?$|\b(?:Member|Adjudicator|Arbitrator|Chairperson|Vice-Chairperson|Commissioner|Associate Judge|Master|Prothonotary|Referee)$"
        )
        .is_match(value)
            || cached_regex!(RELEASE, r"^(?i:released|date of release|dated|i agree)\b").is_match(value))
}

/// A paragraph's end before the signatures and release lines it closes with, which are the
/// decision's; never its first line.
pub(super) fn paragraph_end_before_signatures(text: &ScalarText<'_>, start: usize, end: usize) -> usize {
    let index = text.lines();
    let first = index.partition_point(|line| line[2] <= start);
    let last = index.partition_point(|line| line[2] < end);
    let mut candidate = end;
    for position in (first..last).rev() {
        let line = index[position];
        let value = text.value[line[0]..line[1]].trim();
        if !value.is_empty() && !signature_line(value) {
            break;
        }
        candidate = line[2];
    }
    candidate
}

/// Bare paragraph numbers a provider's flattened text runs into what precedes them on their line:
/// the heading before them ("I. Introduction 1 The complainant …", "Requête visant à rejeter les
/// griefs pour motif d'abandon 6 Le …"), or, where it runs a whole decision onto one line, the
/// paragraph before them ("… for the grievor. 2 The employer …"). Only a count from 1 that runs on
/// through them takes them.
pub(super) fn bare_heading_joined(text: &ScalarText<'_>, known: &HashSet<usize>) -> Vec<Marker> {
    let mut result = Vec::new();
    for line in lines(text) {
        // A line holding a decision's paragraphs, as a provider that drops their breaks gives them.
        let run_on = line.text.len() > 1_500;
        // Each number opening the line or following a space; some providers run a number into its
        // paragraph's first word ("Introduction 1The complainants …"), which a line may open with too.
        let openings = std::iter::once(0).chain(line.text.match_indices(' ').map(|(at, _)| at + 1));
        for number_at in openings {
            let digits = line.text[number_at..].bytes().take(4).take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                continue;
            }
            let spaced = line.text.as_bytes().get(number_at + digits) == Some(&b' ');
            let content = number_at + digits + usize::from(spaced);
            let opens = |value: char| value.is_uppercase() || spaced && ['"', '\u{201c}'].contains(&value);
            if !line.text[content..].chars().next().is_some_and(opens)
                || !spaced && !line.text[content..].chars().nth(1).is_some_and(char::is_lowercase)
                || number_at == 0 && spaced
            {
                continue;
            }
            let start = text.scalar(line.byte_start + number_at);
            if known.contains(&start) {
                continue;
            }
            let before = line.text[..number_at].trim();
            let formal = number_at == 0 || joined_heading_levels(before).is_some();
            let sentence = !formal && sentence_heading(before, &line.text[content..]);
            // The paragraph before may end in a heading run in after its last sentence ("… April 12,
            // 2010. Reasons 20 At the outset …", "… for the complainant? Arguments A) Complainant's
            // arguments 29 According …", "… evidence. B. Did the board …?"); a question heads it
            // alone ("Issue I: Did the respondent abuse its authority …? 83 …"); in a run-on line the
            // paragraph before may simply end.
            let run = !formal && !sentence && {
                let ended = |value: &str| value.ends_with(['.', '?', '!', ':', ';', '"', '\u{201d}', ')', ']', '…']);
                let mut closes = [". ", "? ", "! "].iter()
                    .flat_map(|close| before.match_indices(close).map(|(at, _)| at + 2)).collect::<Vec<_>>();
                closes.sort_unstable();
                run_on && ended(before)
                    || before.ends_with('?') && before.len() <= 300
                    || closes.iter().rev().take(4).map(|at| before[*at..].trim())
                        .any(|heading| !heading.is_empty() && (joined_heading_levels(heading).is_some()
                            || sentence_heading(heading, &line.text[content..])))
            };
            if !(formal || sentence || run) {
                continue;
            }
            result.push(Marker {
                number: line.text[number_at..number_at + digits].parse().unwrap(),
                start,
                content_start: text.scalar(line.byte_start + content),
                style: MarkerStyle::Bare,
                score: if formal { 0.6 } else { 0.35 },
                formal,
                sentence,
            });
        }
    }
    result
}

/// A provider text's paragraphs numbered in parentheses ("(3) I note the decision …"), read only
/// where it has no other count: a quoted provision numbers its subsections the same way.
pub(super) fn parenthesized_paragraphs(text: &ScalarText<'_>, excluded: &[ScalarRange]) -> Vec<Block> {
    let markers = lines(text)
        .filter_map(|line| {
            let lead = leading_ascii_space(line.text);
            let rest = line.text[lead..].strip_prefix('(')?;
            let digits = rest.bytes().take(4).take_while(u8::is_ascii_digit).count();
            if digits == 0 || rest.as_bytes().get(digits) != Some(&b')') {
                return None;
            }
            let after = &rest[digits + 1..];
            let content = after.len() - after.trim_start().len();
            (content > 0 && after.trim_start().chars().next().is_some_and(char::is_uppercase)).then(|| Marker {
                number: rest[..digits].parse().unwrap(),
                start: line.scalar_start,
                content_start: text.scalar(line.byte_start + lead + 1 + digits + 1 + content),
                style: MarkerStyle::Bracket,
                score: 1.0,
                formal: false,
                sentence: false,
            })
        })
        .collect::<Vec<_>>();
    let (chain, _) = rooted_chain(&markers);
    // A decision's own count runs through it, or at least opens it with paragraphs; a list in a decision
    // that numbers no paragraphs stays in a corner of it: an appeal's grounds a line each ("(1) It should
    // have been held …"), or points made late in its reasons.
    if chain.len() < 5 {
        return Vec::new();
    }
    let length = text.utf16_len().max(1) as f64;
    let (first, last) = (text.utf16(chain[0].start) as f64, text.utf16(chain[chain.len() - 1].start) as f64);
    let gap = (last - first) / (chain.len() - 1) as f64;
    if (last - first) / length < 0.4 && (gap < 300.0 || first / length > 0.25 || chain.len() < 10) {
        return Vec::new();
    }
    let mut counts = chain
        .windows(2)
        .map(|pair| word_count(text.slice(pair[0].start..pair[1].start).unwrap_or(""), false))
        .collect::<Vec<_>>();
    if median(&mut counts) < 12.0 {
        return Vec::new();
    }
    paragraph_ranges(text, &chain, &chain, MarkerStyle::Bracket, false, excluded)
}

/// A provider text's paragraphs where it numbers none, a line each: from where its reasons open
/// (an author's "MARTLAND J.—…", "The judgment of the Court was delivered by", else the end of its
/// catalogue record) to its note list, the lines that read as running prose.
pub(super) fn unnumbered_paragraphs(text: &ScalarText<'_>, until: usize) -> Vec<Block> {
    let index = text.lines();
    let value = |line: &[usize; 3]| text.value[line[0]..line[1]].trim();
    let record = index.iter().position(|line| value(line) == "Decision Content").map_or(0, |at| at + 1);
    let author = index.iter().skip(record).position(|line| {
        let line = value(line);
        cached_regex!(AUTHOR, r"^[A-Z][A-Za-zÀ-ÿ'’. -]{1,60}?(?:C\.J\.[A-Z.]*|J\.A\.|JJ\.|J\.)\s*[—–-]\s*\S").is_match(line)
            || line.contains("judgment of the Court was delivered by")
            || line.contains("reasons for judgment were delivered by")
    });
    let prose = |line: &str| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        let lowercase = words.iter().filter(|word| word.chars().next().is_some_and(char::is_lowercase)).count();
        words.len() >= 12 && lowercase >= 4 && line.trim_end().ends_with(['.', '?', '!', ':', ';', '"', '”', ')'])
    };
    // Without an author's line, the reasons open at the first two lines of running prose in a row: the
    // front matter's sentence ("In the matter of a complaint …") stands among its captions alone.
    let from = author.map_or_else(|| {
        let lines = index[record.min(index.len())..].iter().enumerate()
            .filter(|(_, line)| !value(line).is_empty()).collect::<Vec<_>>();
        lines.windows(2).find(|pair| prose(value(pair[0].1)) && prose(value(pair[1].1)))
            .map_or(record, |pair| record + pair[0].0)
    }, |at| record + at);
    index[from.min(index.len())..]
        .iter()
        .filter(|line| line[2] < until)
        .filter(|line| prose(value(line)) && !signature_line(value(line)))
        .map(|line| {
            let lead = text.value[line[0]..line[1]].len() - text.value[line[0]..line[1]].trim_start().len();
            let start = text.scalar(line[0] + lead);
            let end = text.scalar(line[0] + text.value[line[0]..line[1]].trim_end().len());
            Block {
                kind: NodeKind::Prose,
                range: ScalarRange { start, end },
                label: None,
                aliases: Vec::new(),
                parent_label: None,
                content_start: None,
                marker_range: None,
                note: None,
                diagnostic: None,
                source: Derivation::Heuristic,
                origin_id: ENGINE_ORIGIN,
            }
        })
        .collect()
}

pub(super) fn monotone_scopes<'a>(
    markers: impl IntoIterator<Item = &'a Marker>,
    max_gap: u32,
) -> Vec<Vec<Marker>> {
    let mut scopes = Vec::<Vec<Marker>>::new();
    let mut by_last = HashMap::<u32, Vec<usize>>::new();
    for marker in markers {
        let index = (marker.number.saturating_sub(max_gap)..marker.number)
            .flat_map(|value| by_last.get(&value).into_iter().flatten().copied())
            .reduce(|best, current| {
                let left = scopes[current][0].number;
                let right = scopes[best][0].number;
                if left < right || (left == right && current < best) {
                    current
                } else {
                    best
                }
            })
            .unwrap_or(scopes.len());
        if index == scopes.len() {
            scopes.push(vec![marker.clone()]);
        } else {
            let previous = scopes[index].last().unwrap().number;
            if let Some(values) = by_last.get_mut(&previous) {
                values.retain(|value| *value != index);
            }
            scopes[index].push(marker.clone());
        }
        by_last.entry(marker.number).or_default().push(index);
    }
    scopes
}

pub(super) fn contiguous_scopes(markers: &[Marker]) -> Vec<Vec<Marker>> {
    let mut scopes: Vec<Vec<Marker>> = Vec::new();
    for marker in markers {
        if scopes
            .last()
            .and_then(|scope| scope.last())
            .is_some_and(|prior| marker.number == prior.number + 1)
        {
            scopes.last_mut().unwrap().push(marker.clone());
        } else {
            scopes.push(vec![marker.clone()]);
        }
    }
    scopes
}

pub(super) fn recover_contiguous(
    text: &ScalarText<'_>,
    line: Vec<Marker>,
    candidates: &[Marker],
) -> Vec<Marker> {
    if line.is_empty() {
        return line;
    }
    let within = |number: u32, from: usize, to: usize, formal: bool, sentence: bool| {
        let start = candidates.partition_point(|value| value.start <= from);
        let end = candidates.partition_point(|value| value.start < to);
        let mut matching = candidates[start..end].iter().filter(|value| {
            value.number == number && ((!formal || value.formal) && (!sentence || value.sentence))
        });
        let candidate = matching.next()?;
        matching.next().is_none().then_some(candidate)
    };
    let mut recovered = HashMap::<usize, Marker>::new();
    for pair in line.windows(2) {
        if pair[0].number >= pair[1].number {
            continue;
        }
        let mut found = Vec::new();
        for number in pair[0].number + 1..pair[1].number {
            let Some(candidate) = within(number, pair[0].start, pair[1].start, true, false) else {
                found.clear();
                break;
            };
            found.push(candidate);
        }
        for marker in found {
            recovered.insert(marker.start, marker.clone());
        }
        if pair[1].number == pair[0].number + 2 {
            if let Some(candidate) = within(
                pair[0].number + 1,
                pair[0].start,
                pair[1].start,
                false,
                true,
            ) {
                recovered.insert(candidate.start, candidate.clone());
            }
        }
    }
    if let Some(first) = line.first().filter(|value| value.number > 1) {
        let end = candidates.partition_point(|value| value.start < first.start);
        let first_utf16 = text.utf16(first.start);
        let mut matching = candidates[..end].iter().filter(|value| {
            value.number == first.number - 1
                && value.formal
                && first_utf16 - text.utf16(value.start) <= 2_000
        });
        if let Some(candidate) = matching.next() {
            if matching.next().is_none() {
                recovered.insert(candidate.start, candidate.clone());
            }
        }
    }
    let mut result = line;
    result.extend(recovered.into_values());
    result.sort_by_key(|value| value.start);
    result
}

pub(super) fn fill_lossy_marker_gaps(
    text: &ScalarText<'_>,
    spine: &[Marker],
    style: MarkerStyle,
) -> Vec<Marker> {
    let known = spine
        .iter()
        .map(|value| value.start)
        .collect::<HashSet<_>>();
    let known = match style {
        MarkerStyle::Bracket => [Some(&known), None],
        MarkerStyle::Dot => [None, Some(&known)],
        MarkerStyle::Bare => return spine.to_vec(),
    };
    let mut candidates = heading_joined(text, known);
    let candidates = std::mem::take(&mut candidates[style as usize]);
    let mut recovered = HashMap::<u32, Vec<Marker>>::new();
    for candidate in candidates {
        let at = spine.partition_point(|value| value.start < candidate.start);
        let before = spine[..at].last();
        let after = spine.get(at);
        let between = before.zip(after).is_some_and(|(left, right)| {
            left.number < candidate.number && candidate.number < right.number
        });
        let leading = before.is_none()
            && after.is_some_and(|right| {
                candidate.number > 0
                    && candidate.number < right.number
                    && right.number - candidate.number <= 2
                    && text.utf16(right.start) - text.utf16(candidate.start) <= 2_000
            });
        let sentence = before.zip(after).is_some_and(|(left, right)| {
            left.number + 1 == candidate.number && candidate.number + 1 == right.number
        }) && candidate.sentence;
        if (between || leading) && (candidate.formal || sentence) {
            recovered
                .entry(candidate.number)
                .or_default()
                .push(candidate);
        }
    }
    let mut result = spine.to_vec();
    result.extend(
        recovered
            .into_values()
            .filter(|values| values.len() == 1)
            .flatten(),
    );
    result.sort_by_key(|value| value.start);
    result
}

pub(super) fn quoted_dot(text: &ScalarText<'_>, marker: &Marker) -> bool {
    if marker.style != MarkerStyle::Dot {
        return false;
    }
    let start = text.byte(marker.start);
    let end = text.value[start..]
        .find('\n')
        .map_or(text.value.len(), |at| start + at);
    let line = &text.value[start..end];
    cached_regex!(OPEN, r"^\d{1,4}\.\s+\(\d{1,4}\)\s+").is_match(line)
        && cached_regex!(WORD, r"(?i)\b(?:Act|Code|Regulations?|Rules?|shall|must)\b")
            .is_match(line)
}

pub(super) struct Hypothesis {
    style: MarkerStyle,
    markers: Vec<Marker>,
    all: Arc<[Marker]>,
    short: bool,
    score: f64,
}

pub(super) fn paragraph_ranges(
    text: &ScalarText<'_>,
    selected: &[Marker],
    all: &[Marker],
    style: MarkerStyle,
    fill_gaps: bool,
    extra: &[ScalarRange],
) -> Vec<Block> {
    let selected = if fill_gaps && style != MarkerStyle::Bare {
        fill_lossy_marker_gaps(text, selected, style)
    } else {
        selected.to_vec()
    };
    let mut boundaries = all
        .iter()
        .map(|value| value.start)
        .chain(selected.iter().map(|value| value.start))
        .chain(extra.iter().map(|value| value.start))
        .chain([text.len()])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    selected
        .into_iter()
        .map(|marker| {
            let end = paragraph_end_before_trailing_headings(
                text,
                marker.start,
                next_boundary(&boundaries, marker.start, text.len()),
            );
            let mut block = Block::labelled(
                NodeKind::Paragraph,
                format!("par{}", marker.number),
                marker.start,
                end,
            );
            // The number is read from the text, so it is printed there.
            block.marker_range = Some(ScalarRange { start: marker.start, end: marker.content_start });
            block
        })
        .collect()
}

pub(super) fn paragraph_end_before_trailing_headings(
    text: &ScalarText<'_>,
    start: usize,
    end: usize,
) -> usize {
    let index = text.lines();
    let first = index.partition_point(|line| line[2] <= start);
    let last = index.partition_point(|line| line[2] < end);
    let mut candidate = end;
    let mut found = false;
    for position in (first..last).rev() {
        let line = index[position];
        let value = text.value[line[0]..line[1]].trim();
        let preceding = position
            .checked_sub(1)
            .map(|previous| text.value[index[previous][0]..index[previous][1]].trim());
        let separated = preceding == Some("");
        let words = value.split_whitespace().collect::<Vec<_>>();
        let enumerated = words.first().is_some_and(|word| heading_enumerator(word));
        // A question can be a heading; its wording alone does not establish that role.
        // An introductory colon instead connects the following line to its paragraph.
        let heading_context = !preceding.is_some_and(|line| line.ends_with(':'))
            && (enumerated || separated || (!value.ends_with('?') && heading_title_case(&words)));
        if value.is_empty() {
            candidate = line[2];
        } else if heading_context && formal_heading(value) {
            candidate = line[2];
            found = true;
        } else {
            break;
        }
    }
    if found {
        candidate
    } else {
        end
    }
}

/// Statute marginal notes are short sentence-case lines ("Lawyer's right to costs out of property
/// recovered") without terminal punctuation; structural headings ("PART 2", "Division 3") also
/// precede the provision they introduce. Enactment-history lines (which carry digits) stay put.
pub(super) fn provision_end_before_marginal_notes(text: &ScalarText<'_>, start: usize, end: usize) -> usize {
    let index = text.lines();
    let first = index.partition_point(|line| line[2] <= start);
    let last = index.partition_point(|line| line[2] < end);
    let mut candidate = end;
    let mut notes = 0;
    for position in (first..last).rev() {
        let line = index[position];
        let value = text.value[line[0]..line[1]].trim();
        let words = value.split_whitespace().count();
        let structural = cached_regex!(
            VALUE,
            r"^(?:PART|Part|DIVISION|Division|SUBDIVISION|Subdivision|CHAPTER|Chapter|TITLE|Title|SCHEDULE|Schedule|PARTIE|Partie|SECTION|Section)\s+[0-9IVXLC]+[A-Z]?(?:\.\d+)?\b"
        )
        .is_match(value);
        let note = (1..=16).contains(&words)
            && value.chars().next().is_some_and(char::is_uppercase)
            && !value.ends_with(['.', ';', ':', ',', '—', '-'])
            && !value.chars().any(|character| character.is_ascii_digit());
        if value.is_empty() {
            candidate = line[2];
        } else if (note || structural) && notes < 4 {
            candidate = line[2];
            notes += 1;
        } else {
            break;
        }
    }
    if notes > 0 {
        candidate
    } else {
        end
    }
}

pub(super) fn detect_paragraphs(
    text: &ScalarText<'_>,
    profile: DetectionProfile,
    excluded: &[ScalarRange],
    courtlistener: bool,
) -> Vec<Block> {
    let text_length = text.utf16_len();
    let strict = profile != DetectionProfile::CaseLossy;
    let contiguous = profile == DetectionProfile::CaseContiguousComplete;
    let rooted = profile == DetectionProfile::CaseRootedComplete;
    let markers = paragraph_markers(text, contiguous, courtlistener);
    let mut by_style: [Vec<Marker>; 3] = std::array::from_fn(|_| Vec::new());
    for marker in markers {
        if courtlistener {
            let byte = text.byte(marker.start);
            let line_end = text.value[byte..].find('\n').map_or(text.value.len(), |at| byte + at);
            // An isolated unadorned number is layout or an unresolved note, not a paragraph.
            if text.value[byte..line_end].trim().bytes().all(|value| value.is_ascii_digit()) {
                continue;
            }
        }
        let filtered = courtlistener || rooted || (contiguous && marker.style != MarkerStyle::Bare);
        if !filtered || marker_visible(&marker, excluded) && (!strict || !quoted_dot(text, &marker))
        {
            by_style[marker.style as usize].push(marker);
        }
    }
    let mut joined: [Vec<Marker>; 2] = std::array::from_fn(|_| Vec::new());
    if rooted || contiguous {
        let known: [HashSet<usize>; 2] = std::array::from_fn(|index| {
            by_style[index].iter().map(|marker| marker.start).collect()
        });
        joined = heading_joined(
            text,
            [
                (rooted || !by_style[0].is_empty()).then_some(&known[0]),
                (rooted || !by_style[1].is_empty()).then_some(&known[1]),
            ],
        );
    }
    let mut hypotheses = Vec::<Hypothesis>::new();
    for style in [MarkerStyle::Bracket, MarkerStyle::Dot, MarkerStyle::Bare] {
        let style_index = style as usize;
        if rooted {
            let mut candidates = std::mem::take(&mut by_style[style_index]);
            if style != MarkerStyle::Bare {
                candidates.append(&mut joined[style_index]);
            } else {
                let known = candidates.iter().map(|marker| marker.start).collect::<HashSet<_>>();
                candidates.extend(bare_heading_joined(text, &known));
            }
            // A note list numbered as the paragraphs are opens a second count from 1 that runs through
            // the numbers after it; the paragraphs' count is read before it, so it never runs on into
            // the notes. A paragraph quoting another decision's "[1]" to "[3]" opens no such count.
            let list = note_list_start(&candidates).unwrap_or(usize::MAX);
            let mut before = candidates.iter().filter(|marker| marker.start < list).cloned().collect::<Vec<_>>();
            let (mut chain, mut score) = rooted_chain(&before);
            if style == MarkerStyle::Bracket {
                bridge_glued_labels(text, &mut before, &mut chain, &mut score);
                chain.retain(|marker| marker.start < list);
                let held = candidates.iter().map(|marker| marker.start).collect::<HashSet<_>>();
                candidates.extend(before.into_iter().filter(|marker| !held.contains(&marker.start)));
            }
            // A decision of one paragraph numbers it "[1]", and no note call names it.
            let lone = style == MarkerStyle::Bracket && chain.len() == 1 && chain[0].number == 1 && candidates.len() == 1
                && !text.value[..text.byte(chain[0].start)].contains("[1]");
            if chain.len() < 2 && !lone || endnote_shaped(text, &chain) {
                continue;
            }
            if chain.len() >= 5 {
                hypotheses.push(Hypothesis {
                    style,
                    markers: chain,
                    all: candidates.into(),
                    short: false,
                    score,
                });
            } else if style == MarkerStyle::Bracket && sole_chain(&chain, &candidates) {
                hypotheses.push(Hypothesis {
                    style,
                    markers: chain,
                    all: candidates.into(),
                    short: true,
                    score,
                });
            }
            continue;
        }
        let style_markers: Vec<Marker> = if contiguous && style != MarkerStyle::Bare {
            recover_contiguous(
                text,
                std::mem::take(&mut by_style[style_index]),
                &joined[style_index],
            )
            .into_iter()
            .filter(|value| marker_visible(value, excluded))
            .collect()
        } else {
            std::mem::take(&mut by_style[style_index])
        };
        let scopes = if contiguous {
            contiguous_scopes(&style_markers)
        } else {
            monotone_scopes(&style_markers, 8)
        };
        let all: Arc<[Marker]> = style_markers.into();
        for scope in scopes.iter() {
            if scope.len() >= 5 {
                hypotheses.push(Hypothesis {
                    style,
                    markers: scope.clone(),
                    all: Arc::clone(&all),
                    short: false,
                    score: 0.0,
                });
            } else if style == MarkerStyle::Bracket
                && scope.len() >= 2
                && scope
                    .iter()
                    .enumerate()
                    .all(|(index, value)| value.number == index as u32 + 1)
                && (!strict
                    || (scopes
                        .iter()
                        .all(|other| std::ptr::eq(other, scope) || other.len() == 1)
                        && all.iter().all(|value| {
                            scope.iter().any(|mark| mark.start == value.start)
                                || value.number > scope.last().unwrap().number + 1
                        })))
            {
                hypotheses.push(Hypothesis {
                    style,
                    markers: scope.clone(),
                    all: Arc::clone(&all),
                    short: true,
                    score: 0.0,
                });
            }
        }
    }
    if profile != DetectionProfile::CaseRootedComplete
        && hypotheses
            .iter()
            .any(|value| !value.short && value.markers[0].number <= 5)
    {
        hypotheses.retain(|value| value.short || value.markers[0].number <= 5);
    }
    let rank = |style| match style {
        MarkerStyle::Bracket => 2,
        MarkerStyle::Dot => 1,
        MarkerStyle::Bare => 0,
    };
    hypotheses.sort_by(|left, right| {
        left.short.cmp(&right.short).then_with(|| {
            if profile == DetectionProfile::CaseRootedComplete {
                right
                    .score
                    .total_cmp(&left.score)
                    .then(rank(right.style).cmp(&rank(left.style)))
            } else {
                right
                    .markers
                    .len()
                    .cmp(&left.markers.len())
                    .then(rank(right.style).cmp(&rank(left.style)))
                    .then(left.markers[0].number.cmp(&right.markers[0].number))
            }
        })
    });
    // A rooted count's paragraphs end at one another and at a second count from 1, not at the
    // numbers a paragraph quotes from another decision, which stay in the paragraph quoting them.
    let boundaries = |hypothesis: &Hypothesis| -> Vec<Marker> {
        if rooted {
            hypothesis.markers.iter().cloned().chain(restart_boundaries(&hypothesis.all, &hypothesis.markers)).collect()
        } else {
            hypothesis.all.to_vec()
        }
    };
    for hypothesis in hypotheses {
        let mut next = HashMap::with_capacity(hypothesis.all.len());
        for pair in hypothesis.all.windows(2) {
            next.insert(pair[0].start, pair[1].start);
        }
        if let Some(last) = hypothesis.all.last() {
            next.insert(last.start, text.len());
        }
        let first_start = hypothesis.markers[0].start;
        let last_start = hypothesis.markers.last().unwrap().start;
        let mut counts = hypothesis
            .markers
            .iter()
            .map(|marker| {
                let end = next.get(&marker.start).copied().unwrap_or(text.len());
                if end >= marker.start {
                    word_count(
                        text.slice(marker.start..end)
                            .expect("section range is bounded"),
                        contiguous,
                    )
                } else {
                    0
                }
            })
            .collect::<Vec<_>>();
        let bounded = if counts.len() > 1 {
            counts.len() - 1
        } else {
            counts.len()
        };
        let mean = counts[..bounded].iter().sum::<usize>() as f64 / bounded.max(1) as f64;
        let maximum = counts[..bounded].iter().copied().max().unwrap_or(0);
        let median = median(&mut counts[..bounded]);
        // The structure engine scores the unmodified hypothesis. Lossy heading inference
        // changes only the returned ranges after that hypothesis is accepted.
        let first_utf16 = text.utf16(first_start);
        let start = first_utf16 as f64 / text_length.max(1) as f64;
        let span = (text.utf16(last_start) - first_utf16) as f64 / text_length.max(1) as f64;
        if hypothesis.short {
            if text_length <= 6_000
                // A provider's catalogue record opens its text, so a rooted count may start late.
                && (rooted || first_utf16 <= 1_200 || start <= 0.5)
                && counts.iter().copied().max().unwrap_or(0) >= 30
            {
                return paragraph_ranges(
                    text,
                    &hypothesis.markers,
                    &boundaries(&hypothesis),
                    hypothesis.style,
                    !strict,
                    excluded,
                );
            }
            continue;
        }
        let substantive = counts.iter().filter(|value| **value >= 12).count() as f64
            / hypothesis.markers.len() as f64;
        if !(median >= 12.0 || mean >= 20.0 || maximum >= 30)
            || span < 0.05
            || (hypothesis.style == MarkerStyle::Bracket
                && text_length > 6_000
                && start > 0.7
                && substantive < 0.5)
            || (hypothesis.style != MarkerStyle::Bracket && substantive < 0.7)
            || (hypothesis.style == MarkerStyle::Bare
                && (median < 20.0 || span < 0.15 || start > 0.7))
        {
            continue;
        }
        return paragraph_ranges(
            text,
            &hypothesis.markers,
            &boundaries(&hypothesis),
            hypothesis.style,
            !strict,
            excluded,
        );
    }
    if rooted {
        return parenthesized_paragraphs(text, excluded);
    }
    Vec::new()
}

pub(super) fn gapped_paragraphs(blocks: &[Block]) -> bool {
    let mut prior = None;
    blocks
        .iter()
        .filter(|value| value.kind == NodeKind::Paragraph)
        .filter_map(|value| {
            value
                .label
                .as_deref()?
                .strip_prefix("par")?
                .parse::<u32>()
                .ok()
        })
        .any(|value| {
            let gapped = prior.is_some_and(|prior| value != prior + 1);
            prior = Some(value);
            gapped
        })
}

pub(super) fn clipped_case_paragraphs(
    mut blocks: Vec<Block>,
    excluded: &[ScalarRange],
    text: &ScalarText<'_>,
) -> Vec<Block> {
    blocks.retain_mut(|block| {
        for range in excluded {
            if range.start >= block.range.end {
                break;
            }
            if range.end <= block.range.start {
                continue;
            }
            if range.start <= block.range.start {
                return false;
            }
            block.range.end = range.start;
        }
        block.range.end > block.range.start
            && text
                .slice(block.range.start..block.range.end)
                .expect("block range is bounded")
                .chars()
                .any(char::is_alphabetic)
    });
    blocks
}
