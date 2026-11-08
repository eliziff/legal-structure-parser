//! Numbered paragraphs: their markers, the counts they form, and the ranges each count gives.

use super::*;
use crate::judgment::{author_line, signature_line};
use crate::statute::{marginal_note_shaped, markdown_line};

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

/// A paragraph's bracket as a recognizer misreads it: "(2]", "6]", "|7|", "26|", "[11 This": the number and where the
/// marker ends. A list's "(2)" and "2)" stand as they are.
fn misread_bracket(value: &str) -> Option<(&str, usize)> {
    let open = value.chars().next().filter(|c| "[(|{".contains(*c)).map_or(0, char::len_utf8);
    let rest = value[open..].trim_start_matches(' ');
    let (number, length) = decimal_prefix(rest, 4)?;
    let after = rest[length..].trim_start_matches(' ');
    if after.starts_with([']', '|', '}']) {
        return Some((number, value.len() - after.len() + 1));
    }
    (value.starts_with('[') && after.starts_with(char::is_uppercase)).then_some((number, value.len() - after.len()))
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
            // (A recognizer may leave a space inside the brackets: "[ 13]".)
            let rest = rest.trim_start_matches(' ');
            decimal_prefix(rest, 4).and_then(|(number, length)| {
                // A table of contents' line names the paragraphs under a heading ("[1535] - [1539]"); it opens none.
                let after = rest[length..].trim_start_matches(' ').strip_prefix(']')?;
                (!cached_regex!(TOC_RANGE, r"^\s*[-\u{2013}\u{2014}]\s*\[\d{1,4}\]\s*$").is_match(after))
                    .then(|| (number, MarkerStyle::Bracket, value.len() - after.len()))
            }).or_else(|| misread_bracket(value).map(|(number, end)| (number, MarkerStyle::Bracket, end)))
        } else if let Some((number, end)) = misread_bracket(value) {
            Some((number, MarkerStyle::Bracket, end))
        } else {
            decimal_prefix(value, 4).and_then(|(number, length)| {
                let rest = &value[length..];
                // An ordinal numbers too, as older papers print their articles: "1st.", "2d.", "4th.".
                let ordinal = ["st", "nd", "rd", "th", "d"].iter().find_map(|suffix| rest.strip_prefix(suffix))
                    .filter(|after| after.starts_with('.') && after[1..].chars().next().is_some_and(char::is_whitespace));
                if let Some(after) = ordinal {
                    return Some((number, MarkerStyle::Dot, value.len() - after.len() + 1));
                }
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
        if let Some((number, style, marker_end)) = basic.filter(|(_, style, marker_end)| *style != MarkerStyle::Bare || !cited_figure(&value[*marker_end..])) {
            let content = marker_end + leading_ascii_space(&value[marker_end..]);
            result.push(Marker {
                introduced: text.value[..line.byte_start].trim_end().ends_with(':'),
                going_on: value[content..].starts_with(char::is_lowercase),
                ..Marker::new(number.parse().unwrap(), start, line.scalar_start + lead + shift + content, style, 1.0)
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
                        let content = text.scalar(line.byte_start + line.text.len() - rest.len() + length);
                        result.push(Marker::new(number.parse().unwrap(), start, content, MarkerStyle::Dot, 1.0));
                    }
                }
            }
        }
    }
    result
}

/// Whether a bare figure before `rest` is a citation's or a date's, no paragraph's, at a line's opening or within it: a volume, its
/// report's name after it ("[1989]" / "1 R.C.S. 265", "2 W.W.R. 40", "149 Sask. R. 68", "1 Ch. 264", "1 All E.R. 568", "2 SCR 3", "8 R.N.-B.
/// (2e)", "60 C.C.C (3d)", "923 F.2d 774", "47 Alta. L.R. (4th) 6", "73 C.B.R. (N.S.) 24"), its page and its year or court ("... 401 U.S." / "1 (2009);", "1 (WL Can.)", "101 (N.W.T.S.C.);"), or a
/// day, its month and year after it ("... was rendered on" / "11 September 2025: see ..."). (Brackets number on: "[13] CPR 13.07 provides:",
/// and so does a section's lone letter: "99 S. 24(2) Analysis:".)
fn cited_figure(rest: &str) -> bool {
    cached_regex!(REPORT_NAME, r"^\s*(?:All\s+)?(?:(?:SCR|RCS|FCR|RCF|FC|FCA|FTR|CCC|CR|CRR|DLR|WWR|OR|OAC|BCLR|BCR|BCAC|BCCA|BCSC|WAC|AR|ACWS|WCB|NR|NSR|NBR|QAC|RJQ|CCLT|MPLR|ETR|RFL|CPR|CTC|DTC|LAC|QB|KB|AC|WLR|ER|US|SCC|ONCA|ONSC|ABCA|ABQB|QCCA|QCCS|TCC|MBCA|SKCA|NSCA|NBCA)\s+\(?\d|(?:[A-Z][a-z]+\.|(?:-?[A-Z][a-z]*\.){2,})(?:\s*-?[A-Z]?[a-z]*\.){0,4}(?:[A-Z]\b)?\s*\(?(?:\d|N\.S\.)|(?:[A-Z]\.){1,2}\d[a-z]{1,2}\b|\(\d{4}\)[;,.]|\((?:WL\b|(?:[A-Z]\.){2,}[A-Z.]*(?:\s+[A-Z][a-z]+\.)?\)))").is_match(rest)
        || cached_regex!(WRAPPED_DATE, r"^\s*(?:January|February|March|April|May|June|July|August|September|October|November|December|janvier|février|mars|avril|mai|juin|juillet|août|septembre|octobre|novembre|décembre),?\s+\d{4}\b").is_match(rest)
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

pub(super) fn median(values: &[usize]) -> f64 {
    legal_structure_model::layout::median(values.iter().map(|&value| value as f64).collect())
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
            let formal = (style == MarkerStyle::Bracket || !heading.contains('.')) && formal_heading(heading);
            let sentence = style == MarkerStyle::Bracket
                && sentence_heading(heading, &text.value[line.byte_start + end..]);
            if formal || sentence {
                let score = if formal { 0.6 } else { 0.35 };
                result[index].push(Marker { formal, sentence, ..Marker::new(line.text[digits..tail].parse().unwrap(), start, text.scalar(line.byte_start + end), style, score) });
            }
            at = end;
        }
    }
    result
}

/// A count whose first numbers its reader lost ("1" and "2" read into a citation, "1" run into the small capital
/// opening the judge's name: "1I" "ACOBUCCI J."), the count going on from "3", is read as a lossy reading reads one:
/// the longest count by steps of up to eight (`monotone_scopes`), where it runs more than twice the rooted count; its
/// score as a rooted count's (each number's, and a third more for each that follows the last by one).
pub(super) fn lost_root(chain: &mut Vec<Marker>, score: &mut f64, candidates: &[Marker]) {
    let mut sorted = candidates.to_vec();
    sorted.sort_by_key(|marker| marker.start);
    // The best count, not the first that fits: a stray figure ahead of the next number takes no step from it.
    let mut best = Vec::<Option<(f64, Option<usize>)>>::with_capacity(sorted.len());
    let mut at_number = HashMap::<u32, Vec<usize>>::new();
    for (at, marker) in sorted.iter().enumerate() {
        let root = (marker.number <= LOST_ROOT_START).then_some((marker.score, None));
        let steps = (marker.number.saturating_sub(8)..marker.number).flat_map(|number| at_number.get(&number).into_iter().flatten().copied());
        let onward = steps.filter_map(|before| {
            let reached = best[before]?.0 + marker.score + if sorted[before].number + 1 == marker.number { 0.3 } else { 0.0 };
            Some((reached, Some(before)))
        });
        best.push(root.into_iter().chain(onward).max_by(|a, b| a.0.total_cmp(&b.0)));
        at_number.entry(marker.number).or_default().push(at);
    }
    let last = (0..sorted.len()).filter(|&at| best[at].is_some()).max_by(|&a, &b| best[a].map_or(0.0, |x| x.0).total_cmp(&best[b].map_or(0.0, |x| x.0)));
    let scope = last.map(|last| {
        let mut scope = std::iter::successors(Some(last), |&at| best[at].and_then(|(_, before)| before)).map(|at| sorted[at].clone()).collect::<Vec<_>>();
        scope.reverse();
        scope
    });
    if let Some(scope) = scope {
        // A count that lost its first numbers goes on from near its start, as a run of years ("1999" to "2010") never does;
        // it stands for the count where it reads better than the rooted one (a scan's stray "1" late in the text roots a
        // count that skips most of the decision).
        // And it counts: most of its steps are by one, where a scatter of citations' figures ("1 S.C.R. 3", "17 O.R.")
        // climbs by jumps.
        let counting = scope.windows(2).filter(|pair| pair[1].number == pair[0].number + 1).count() * 2 >= scope.len();
        if scope[0].number <= LOST_ROOT_START && counting && chain_score(&scope) > chain_score(chain) {
            *score = chain_score(&scope);
            *chain = scope;
        }
    }
}

/// A count of bare numbers whose "1" a recognizer lost, or found on a figure in the matter before the reasons ("1 Ad. & E. 3",
/// "1 Halsbury's Statutes"): its first paragraph opens at the last author's line before its "2" ("CORY J. - In this case").
/// A "1" on or after that line stands.
fn rooted_on_author(text: &ScalarText<'_>, chain: &mut Vec<Marker>) {
    let Some(second) = chain.iter().position(|marker| marker.number == 2).filter(|&at| at <= 1) else { return };
    let (before, style) = (chain[second].start, chain[second].style);
    let author = text.lines().iter().rev().map(|line| (line[0], text.value[line[0]..line[1]].trim_end()))
        .find(|(start, line)| text.scalar(*start) < before && author_line(line.trim_start()));
    let Some((byte, line)) = author else { return };
    // Who gives the reasons is no paragraph's words: it opens after "delivered by", "rendu par", on the line or on the next.
    let given = ["delivered by", "rendu par", "rendus par"].iter().filter_map(|words| line.find(words).map(|at| at + words.len())).max();
    let start = match given {
        Some(end) if !line[end..].trim().is_empty() => text.scalar(byte + end + line[end..].len() - line[end..].trim_start().len()),
        Some(_) => {
            let Some(next) = text.lines().iter().find(|other| other[0] > byte && !text.value[other[0]..other[1]].trim().is_empty()) else { return };
            text.scalar(next[0])
        }
        None => text.scalar(byte),
    };
    if start >= before {
        return;
    }
    if second == 1 && chain[0].start >= start {
        return;
    }
    let root = Marker::new(1, start, start, style, 1.0);
    if second == 1 {
        chain[0] = root;
    } else {
        chain.insert(0, root);
    }
}

/// The highest number a count whose first numbers its reader lost may go on from.
const LOST_ROOT_START: u32 = 9;

/// A count's score as it stands: each number's, and a third more for each that follows the last by one.
pub(super) fn chain_score(chain: &[Marker]) -> f64 {
    chain.iter().map(|marker| marker.score).sum::<f64>() + 0.3 * chain.windows(2).filter(|pair| pair[1].number == pair[0].number + 1).count() as f64
}

/// The count from 1 that `candidates` read, each number the last plus one, and its score; another writing's
/// paragraphs a paragraph quotes take no part (`quoted_runs`).
pub(super) fn rooted_chain(candidates: &[Marker]) -> (Vec<Marker>, f64) {
    let quoted = quoted_runs(candidates);
    let selected = select_numeric_sequence(
        candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| !quoted.contains(index))
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
    let mut chain: Vec<Marker> = selected.indices.into_iter().map(|index| candidates[index].clone()).collect();
    whole_list(&mut chain, candidates);
    // The count opens at the last "1" before its "2": an earlier one is a provision or a list in the matter before the
    // reasons.
    if let [first, second, ..] = chain.as_mut_slice() {
        let later = candidates.iter().filter(|marker| marker.number == 1 && marker.style == second.style && first.start < marker.start && marker.start < second.start).max_by_key(|marker| marker.start);
        if let Some(later) = later {
            *first = later.clone();
        }
    }
    (chain, selected.score)
}

/// A count stitched across two lists ("1." to "4." of the agreed facts, then "5." to "8." of the grounds, whose own "1."
/// to "4." it passed over) reads the list it ended in whole: a member standing right after the number before it, which is
/// not the count's member, shows the count changed lists there. A bracketed count is left: the run before its member
/// may be a quotation (`quoted_runs`).
fn whole_list(chain: &mut [Marker], candidates: &[Marker]) {
    let mut sorted = candidates.iter().filter(|marker| marker.style != MarkerStyle::Bracket).collect::<Vec<_>>();
    sorted.sort_by_key(|marker| marker.start);
    for at in 1..chain.len() {
        let place = sorted.partition_point(|marker| marker.start < chain[at].start);
        let run = sorted[..place].iter().rev().zip((0..chain[at].number).rev()).take_while(|(marker, number)| marker.number == *number).map(|(marker, _)| *marker);
        let run = run.collect::<Vec<_>>();
        // (A list's run is a run: one number printed twice is none.)
        if run.len() >= 2 && run.first().is_some_and(|before| before.start != chain[at - 1].start) {
            for marker in run {
                if let Some(member) = chain[..at].iter_mut().find(|member| member.number == marker.number) {
                    *member = marker.clone();
                }
            }
        }
    }
}

/// Another writing's paragraph numbers a paragraph quotes where they repeat its own count's (indexes of `candidates`).
/// Writings nest: a quotation counts on its own, a quotation inside it on its own, and each goes on where the one inside
/// it stops ("[38] … where I stated:", "[32] In Friesen …:", "[37] …", "[38] …", "[39] …", "[18] …" to "[23] …", "[40] …",
/// "[33] …", then the judgment's own "[39] The pernicious …"). So each number goes on the innermost count it follows, or
/// opens a count of its own, and the outermost count is the judgment's. A count still open at the end took the judgment's
/// numbers on (a number printed twice by mistake, its second printing's words the first's paragraph's), or opened at 1
/// and is the judgment's whole. Only a number the judgment's count also holds is quoted: a number nothing else prints is
/// the count's own, however it broke the count.
fn quoted_runs(candidates: &[Marker]) -> HashSet<usize> {
    let mut quoted = HashSet::new();
    for style in [MarkerStyle::Bracket, MarkerStyle::Dot, MarkerStyle::Bare] {
        let mut order = (0..candidates.len()).filter(|&at| candidates[at].style == style).collect::<Vec<_>>();
        order.sort_by_key(|&at| candidates[at].start);
        let next = |count: &[usize]| count.last().map_or(1, |&at| candidates[at].number + 1);
        let (mut counts, mut closed) = (vec![Vec::<usize>::new()], Vec::<usize>::new());
        // (A count takes up again over those inside it, never over a longer one: a stray small number closes no reasons.)
        for (step, &at) in order.iter().enumerate() {
            let resumes = |place: usize| next(&counts[place]) == candidates[at].number && counts[place + 1..].iter().all(|inner| inner.len() <= counts[place].len());
            // (Where two counts could go on, a quotation's gives way to the count it stands in, but for a number printed again
            // before the count has gone past it: that printing is the count's, this the quotation's.)
            let places = (0..counts.len()).rev().filter(|&place| resumes(place)).collect::<Vec<_>>();
            let introduced = |place: usize| counts[place].first().is_some_and(|&first| candidates[first].introduced);
            let number = candidates[at].number;
            let again = || order[step + 1..].iter().map(|&later| candidates[later].number).take_while(|&later| later <= number + 8).any(|later| later == number);
            // (A number a colon introduces, printed again with no colon over it, is a quotation's. So is one over words going on,
            // a number misfired on a line of a sentence: "[15]" / "of Stockwell Day, ...", "... of Appendix" / "2 of the Referendum
            // Act".)
            let plainly = || order[step + 1..].iter().take_while(|&&later| candidates[later].number <= number + 8).any(|&later| candidates[later].number == number && !candidates[later].introduced && !candidates[later].going_on);
            if (candidates[at].introduced || candidates[at].going_on) && plainly() {
                counts.push(vec![at]);
                continue;
            }
            let inner = places.first().copied().filter(|&place| introduced(place) && again());
            match inner.or(places.iter().copied().find(|&place| !introduced(place))).or(places.first().copied()) {
                Some(count) => {
                    closed.extend(counts.drain(count + 1..).flatten());
                    counts[count].push(at);
                }
                None => counts.push(vec![at]),
            }
        }
        let mut own = counts.remove(0);
        for count in counts {
            if candidates[count[0]].number == 1 && count.len() > own.len() {
                closed.append(&mut own);
                own = count;
                continue;
            }
            // A quotation's numbers printed before the judgment's own, the judgment's going on past them: ": [89] [90] [91] ...
            // [89] [90] [91] [92]".
            let reprinted = own.iter().filter(|&&at| candidates[at].number >= candidates[count[0]].number).count();
            // (One number will do: "... observed:" / "[113] There are ..." / "[113] In R. v. Loor ...".)
            if reprinted >= 1 && reprinted < own.len() && candidates[own[own.len() - reprinted]].introduced && !candidates[count[0]].introduced && count.iter().any(|&at| candidates[at].number >= next(&own)) {
                let quotation = own.split_off(own.len() - reprinted);
                closed.extend(quotation);
            }
            let reached = next(&own);
            let (more, rest): (Vec<usize>, Vec<usize>) = count.into_iter().partition(|&at| candidates[at].number >= reached);
            own.extend(more);
            closed.extend(rest);
        }
        let numbers = own.iter().map(|&at| candidates[at].number).collect::<HashSet<_>>();
        quoted.extend(closed.into_iter().filter(|&at| numbers.contains(&candidates[at].number)));
    }
    quoted
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
        // A text layer that lost the opening bracket ("5] In February 1999 …").
        let opening = line.text.trim_start();
        let lead = line.text.len() - opening.len();
        let length = opening.bytes().take(4).take_while(u8::is_ascii_digit).count();
        if length > 0 && matches!(opening.as_bytes().get(length), Some(b']' | b'}')) {
            let following = opening[length + 1..].trim_start_matches([' ', '\t']);
            let start = text.scalar(line.byte_start + lead);
            if following.starts_with(char::is_uppercase) && !held.contains(&start) {
                glued.push(Marker::new(opening[..length].parse().unwrap(), start, text.scalar(line.byte_start + lead + length + 1), MarkerStyle::Bracket, 0.2));
            }
        }
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
            // A Word bookmark's id an export left before the paragraph's words ("[2] 30BThe plaintiff …").
            let bookmark = following.bytes().take(3).take_while(u8::is_ascii_digit).count();
            let following = match following[bookmark..].strip_prefix('B') {
                Some(rest) if bookmark > 0 && rest.starts_with(char::is_uppercase) => rest,
                _ => following,
            };
            let number = digits[..length].parse::<u32>().unwrap();
            let start = text.scalar(line.byte_start + at);
            if !following.chars().next().is_some_and(|value| value.is_uppercase()
                || ['"', '\u{201c}', '\u{2018}', '\'', '\u{ab}'].contains(&value))
            {
                continue;
            }
            if !held.contains(&start) {
                glued.push(Marker::new(number, start, text.scalar(line.byte_start + end), MarkerStyle::Bracket, 0.2));
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
/// stray "1" in the front matter before the paragraphs' own count opens no first count, and a list
/// sits in the text's tail, most of its numbers in the last quarter: a table of contents counting
/// "[1]" to "[5]" before the reasons is no note list of the paragraphs after it, nor is a list a
/// paragraph quotes ("they include: [1] whether …" to "[4]") with the paragraphs' "[5]" after it.
pub(super) fn note_list_start(text: &ScalarText<'_>, candidates: &[Marker]) -> Option<usize> {
    let length = text.utf16_len();
    let mut sorted = candidates.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|marker| marker.start);
    // The markers a count from 1 takes, in order.
    let run = |markers: &[&Marker]| {
        let mut taken = Vec::new();
        for marker in markers {
            if marker.number as usize == taken.len() + 1 {
                taken.push(marker.start);
            }
        }
        taken
    };
    sorted.iter().enumerate().skip(1).rev().filter(|(_, marker)| marker.number == 1).find_map(|(at, marker)| {
        let (before, after) = sorted.split_at(at);
        let (list, first) = (run(after), run(before).len());
        let tail = list.iter().filter(|start| text.utf16(**start) * 4 > length * 3).count();
        // The paragraphs' count going on after it ("[25]", a quoted "[1]" to "[7]", "[26]") makes it quoted:
        // a number after it beyond the count before it that the list does not take. The count before it
        // is read from line openings alone, so it may stop short where a paragraph's number is glued.
        let continued = || after.iter().any(|marker| marker.number as usize > first && !list.contains(&marker.start));
        (list.len() >= 3 && list.len() * 2 >= after.len() && tail * 10 >= list.len() * 7 && first >= 3 && !continued())
            .then_some(marker.start)
    })
}

/// A rooted count carried past numbers its provider dropped ("16", "17", "19", "21" …): where no
/// candidate after the count's last paragraph carries its next number, the count goes on at the next it
/// finds within three more, if the three numbers from there follow in order.
fn extend_over_gaps(chain: &mut Vec<Marker>, candidates: &[Marker]) {
    let mut sorted = candidates.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|marker| marker.start);
    while let Some(last) = chain.last().cloned() {
        let after = sorted.iter().filter(|marker| marker.start > last.start).collect::<Vec<_>>();
        if after.iter().any(|marker| marker.number == last.number + 1) {
            return;
        }
        let Some(at) = after.iter().position(|marker| (last.number + 2..=last.number + 4).contains(&marker.number)) else { return };
        let mut run = vec![(**after[at]).clone()];
        for marker in &after[at + 1..] {
            if marker.number == run[run.len() - 1].number + 1 {
                run.push((***marker).clone());
            }
        }
        if run.len() < 3 {
            return;
        }
        chain.extend(run);
    }
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
/// matters." under [76]) open none, nor does a list the paragraph introduces ("… are as follows:" then "1.").
pub(super) fn restart_boundaries(text: &ScalarText<'_>, candidates: &[Marker], chain: &[Marker]) -> Vec<Marker> {
    let held = chain.iter().map(|marker| marker.start).collect::<HashSet<_>>();
    let mut rest = candidates.iter().filter(|marker| !held.contains(&marker.start)).collect::<Vec<_>>();
    rest.sort_by_key(|marker| marker.start);
    // A "1" the line before leads into opens no count: a list ("... are as follows:"), or the sentence going on over the
    // line ("[1990]" / "1 R.C.S. 158", "... the s." / "1 analysis", "... of the" / "1 percent").
    let introduced = |marker: &Marker| text.byte_at_scalar(marker.start).is_some_and(|at| {
        let before = text.value[..at].trim_end();
        let last = before.split_whitespace().last().unwrap_or_default();
        let year = last.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')).is_some_and(|inner| inner.len() == 4 && inner.bytes().all(|byte| byte.is_ascii_digit()));
        let word = last.trim_end_matches('.');
        before.ends_with(':') || year || before.ends_with([',', ';']) || before.ends_with(char::is_lowercase)
            || last.ends_with('.') && word.chars().count() <= 2
    });
    rest.iter().enumerate().filter(|(index, marker)| {
        marker.number == 1 && !introduced(marker) && rest[index + 1..].iter().fold(2, |expected, later| {
            if later.number == expected { expected + 1 } else { expected }
        }) > 3
    }).map(|(_, marker)| (*marker).clone()).collect()
}

/// Whether text reads as a heading run in before a paragraph's number: words, short, no list of
/// citations, and no closed sentence unless a question ("Issue I: Did the respondent …?").
pub(crate) fn run_in_heading(value: &str) -> bool {
    value.len() <= 240
        && !value.contains(';')
        && value.split_whitespace().any(|word| word.chars().filter(|c| c.is_alphabetic()).count() >= 3)
        && (!sentence_closed(value) || value.ends_with('?'))
}

/// Bare paragraph numbers a provider runs into what precedes them on their line ("I. Introduction 1 The
/// complainant …", "Issue I: The Tribunal's jurisdiction … 13 Section …", "… for the grievor. 2 The
/// employer …") or into the paragraph's first word ("Introduction 1The complainants …"). What precedes
/// one is a short run-in heading, the words after the last sentence's close, or, in a line holding a
/// decision's paragraphs, the close of the paragraph before. Only a count from 1 that runs on through
/// them takes them.
pub(super) fn bare_heading_joined(text: &ScalarText<'_>, known: &HashSet<usize>) -> Vec<Marker> {
    let mut result = Vec::new();
    for line in lines(text) {
        // A line holding a decision's paragraphs, as a provider that drops their breaks gives them.
        let run_on = line.text.len() > 1_500;
        let openings = std::iter::once(0).chain(line.text.match_indices(' ').map(|(at, _)| at + 1));
        for number_at in openings {
            let digits = line.text[number_at..].bytes().take(4).take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                continue;
            }
            let spaced = line.text.as_bytes().get(number_at + digits) == Some(&b' ');
            let content = number_at + digits + usize::from(spaced);
            let mut word = line.text[content..].chars();
            let (first, second) = (word.next(), word.next());
            // Spaced, the paragraph opens with a capital or a quotation; run in, with a capitalized word
            // ("1The", "37A few").
            let opens = first.is_some_and(char::is_uppercase)
                && (spaced || second.is_some_and(|value| value.is_lowercase() || value == ' '))
                || spaced && first.is_some_and(|value| ['"', '\u{201c}'].contains(&value));
            // A line opening with a spaced number is the line-opening reader's.
            if !opens || number_at == 0 && spaced || cited_figure(&line.text[content..]) {
                continue;
            }
            let start = text.scalar(line.byte_start + number_at);
            if known.contains(&start) {
                continue;
            }
            let before = line.text[..number_at].trim();
            let last_close = [". ", "? ", "! "].iter().filter_map(|close| before.rfind(close)).max();
            let tail = last_close.map_or(before, |at| before[at + 2..].trim());
            // The whole run-in line may be the heading where an abbreviation reads as a sentence's close
            // ("1. Indefinite suspension without pay (PSLREB File No. 566-34-3750) 101 The employer …").
            let candidates = [before, tail];
            let formal = number_at == 0 || candidates.iter().any(|value| run_in_heading(value) && joined_heading_levels(value).is_some());
            if !(formal || candidates.iter().any(|value| run_in_heading(value)) || run_on && sentence_closed(before)) {
                continue;
            }
            let number = line.text[number_at..number_at + digits].parse().unwrap();
            let score = if formal { 0.6 } else { 0.35 };
            result.push(Marker { formal, sentence: !formal, ..Marker::new(number, start, text.scalar(line.byte_start + content), MarkerStyle::Bare, score) });
        }
    }
    result
}

/// A provider text's paragraphs numbered in parentheses ("(3) I note the decision …"), read only
/// where it has no other count: a quoted provision numbers its subsections the same way.
pub(super) fn parenthesized_paragraphs(text: &ScalarText<'_>, excluded: &[ScalarRange]) -> Vec<StructureNode> {
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
            (content > 0 && after.trim_start().chars().next().is_some_and(char::is_uppercase)).then(|| {
                Marker::new(rest[..digits].parse().unwrap(), line.scalar_start, text.scalar(line.byte_start + lead + 1 + digits + 1 + content), MarkerStyle::Bracket, 1.0)
            })
        })
        .collect::<Vec<_>>();
    // And a number a provider runs into the heading before it ("B. SADI Agreement (14) On March 30, 2009 …").
    let mut markers = markers;
    for line in lines(text) {
        for (at, _) in line.text.match_indices(" (") {
            let rest = &line.text[at + 2..];
            let digits = rest.bytes().take(4).take_while(u8::is_ascii_digit).count();
            if digits == 0 || !rest[digits..].starts_with(") ") {
                continue;
            }
            let content = at + 2 + digits + 2;
            let before = line.text[..at].trim();
            if !line.text[content..].starts_with(char::is_uppercase)
                || joined_heading_levels(before).is_none() && !sentence_heading(before, &line.text[content..])
            {
                continue;
            }
            let (start, content) = (text.scalar(line.byte_start + at + 1), text.scalar(line.byte_start + content));
            markers.push(Marker { formal: true, ..Marker::new(rest[..digits].parse().unwrap(), start, content, MarkerStyle::Bracket, 0.6) });
        }
    }
    markers.sort_by_key(|marker| marker.start);
    let (chain, _) = rooted_chain(&markers);
    // A decision's own count runs through it, or numbers only its opening paragraphs ("(1)" to "(7)", the
    // rest unnumbered); a list in a decision that numbers no paragraphs stays in a corner of it: an
    // appeal's grounds a line each ("(1) It should have been held …"), or points made late in its reasons.
    if chain.len() < 5 {
        return Vec::new();
    }
    let length = text.utf16_len().max(1) as f64;
    let (first, last) = (text.utf16(chain[0].start) as f64, text.utf16(chain[chain.len() - 1].start) as f64);
    let gap = (last - first) / (chain.len() - 1) as f64;
    let opening = (last - first) / length < 0.4;
    if opening && (gap < 300.0 || first / length > 0.25) {
        return Vec::new();
    }
    let mut counts = chain
        .windows(2)
        .map(|pair| word_count(text.slice(pair[0].start..pair[1].start).unwrap_or(""), false))
        .collect::<Vec<_>>();
    if median(&mut counts) < 12.0 {
        return Vec::new();
    }
    let mut blocks = paragraph_ranges(text, &chain, &chain, MarkerStyle::Bracket, false, excluded);
    if opening {
        // The last numbered paragraph is its line; the paragraphs after it are unnumbered, up to a note list.
        let index = text.lines();
        let last_start = chain[chain.len() - 1].start;
        let line = index[index.partition_point(|line| line[2] <= last_start) - 1];
        let end = text.scalar(line[0] + text.value[line[0]..line[1]].trim_end().len());
        for block in blocks.iter_mut().filter(|block| block.kind == NodeKind::Paragraph && block.range.start >= last_start) {
            block.range.end = end.max(block.range.start);
        }
        let notes = index.iter().rposition(|line| line[2] > end && {
            let value = text.value[line[0]..line[1]].trim();
            value.starts_with("[1]") || value == "Footnote 1"
        }).map_or(text.len(), |at| index[at][2]);
        blocks.extend(unnumbered_paragraphs(text, end, notes));
    }
    blocks
}

/// Whether a line reads as running prose: a dozen words or more, a few of them in lower case, ending as a sentence does.
pub(super) fn running_prose(line: &str) -> bool {
    let words = line.split_whitespace().collect::<Vec<_>>();
    let lowercase = words.iter().filter(|word| word.chars().next().is_some_and(char::is_lowercase)).count();
    words.len() >= 12 && lowercase >= 4 && line.trim_end().ends_with(['.', '?', '!', ':', ';', '"', '”', ')'])
}

/// A provider text's paragraphs where it numbers none, a line each: from where its reasons open
/// (an author's "MARTLAND J.—…", "The judgment of the Court was delivered by", else the end of its
/// catalogue record), or from `after`, to its note list at `until`, the lines that read as running prose.
pub(super) fn unnumbered_paragraphs(text: &ScalarText<'_>, after: usize, until: usize) -> Vec<StructureNode> {
    let index = text.lines();
    let value = |line: &[usize; 3]| text.value[line[0]..line[1]].trim();
    let record = index.iter().position(|line| value(line) == "Decision Content").map_or(0, |at| at + 1);
    let author = index.iter().skip(record).position(|line| author_line(value(line)));
    let prose = running_prose;
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
        .filter(|line| line[2] >= after && line[2] < until)
        .filter(|line| prose(value(line)) && !signature_line(value(line)))
        .map(|line| {
            let lead = text.value[line[0]..line[1]].len() - text.value[line[0]..line[1]].trim_start().len();
            let start = text.scalar(line[0] + lead);
            let end = text.scalar(line[0] + text.value[line[0]..line[1]].trim_end().len());
            prose_block(ScalarRange { start, end })
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
) -> Vec<StructureNode> {
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
    let last = selected.iter().map(|marker| marker.start).max();
    selected
        .into_iter()
        .map(|marker| {
            let end = paragraph_end(
                text,
                marker.start,
                next_boundary(&boundaries, marker.start, text.len()),
                Some(marker.start) == last,
            );
            let mut block = crate::labelled_node(
                NodeKind::Paragraph,
                format!("{}", marker.number),
                marker.start,
                end,
            );
            // The number is read from the text, so it is printed there.
            let printed = text.slice(marker.start..marker.content_start)
                .expect("printed paragraph marker is bounded").trim_end();
            block.marker_range = Some(ScalarRange { start: marker.start, end: marker.start + printed.chars().count() });
            block
        })
        .collect()
}

/// Where a numbered paragraph opening at `start` ends, `end` being where the next paragraph's number stands or the
/// text ends: after its own lines (`paragraph_tail`), each line read from the paragraph's number and up to `end`
/// (the next number may follow a heading on its line, "(3) Legal proceedings [20] On April 14, …"). `last`: the
/// paragraph is its count's last.
pub(super) fn paragraph_end(text: &ScalarText<'_>, start: usize, end: usize, last: bool) -> usize {
    let index = text.lines();
    let first = index.partition_point(|line| line[2] <= start).saturating_sub(1);
    let (start_byte, end_byte) = (text.byte_at_scalar(start).unwrap_or(0), text.byte_at_scalar(end).unwrap_or(text.value.len()));
    // Each line's words, the scalar its piece of the paragraph opens at, and whether a blank line stands before it.
    let (mut pieces, mut separated, mut blank) = (Vec::<(usize, &str)>::new(), Vec::new(), false);
    for line in &index[first..] {
        if line[0] >= end_byte {
            break;
        }
        let from = line[0].max(start_byte);
        let value = &text.value[from..line[1].min(end_byte).max(from)];
        if value.trim().is_empty() {
            blank = true;
        } else {
            pieces.push((if from > line[0] { start } else { line[2] }, value));
            separated.push(std::mem::take(&mut blank));
        }
    }
    let mut stop = text.value.len().min(end_byte + 200);
    while !text.value.is_char_boundary(stop) {
        stop -= 1;
    }
    let after = &text.value[end_byte..stop.max(end_byte)];
    let lines = pieces.iter().map(|(_, value)| *value).collect::<Vec<_>>();
    pieces.get(paragraph_tail(&lines, &separated, after, last)).map_or(end, |(at, _)| *at)
}

/// Statute marginal notes are short sentence-case lines ("Lawyer's right to costs out of property
/// recovered") without terminal punctuation; structural headings ("PART 2", "Division 3") also
/// precede the provision they introduce. Enactment-history lines (which carry digits) stay put.
/// A line a provider sets in bold alone is a note whatever its words ("**Target — 2050**",
/// "**Liability of directors, officers, etc.**").
pub(super) fn provision_end_before_marginal_notes(text: &ScalarText<'_>, start: usize, end: usize) -> usize {
    let index = text.lines();
    let first = index.partition_point(|line| line[2] <= start);
    let last = index.partition_point(|line| line[2] < end);
    let mut candidate = end;
    let mut notes = 0;
    for position in (first..last).rev() {
        let line = index[position];
        let printed = &text.value[line[0]..line[1]];
        let (value, _) = markdown_line(printed);
        let structural = cached_regex!(
            VALUE,
            r"^(?:PART|Part|DIVISION|Division|SUBDIVISION|Subdivision|CHAPTER|Chapter|TITLE|Title|SCHEDULE|Schedule|PARTIE|Partie|SECTION|Section)\s+[0-9IVXLC]+[A-Z]?(?:\.\d+)?\b"
        )
        .is_match(value);
        let note = marginal_note_shaped(printed);
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
) -> Vec<StructureNode> {
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
    let mut pooled = Vec::<Marker>::new();
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
            let list = note_list_start(text, &candidates).unwrap_or(usize::MAX);
            let mut before = candidates.iter().filter(|marker| marker.start < list).cloned().collect::<Vec<_>>();
            let (mut chain, mut score) = rooted_chain(&before);
            if style == MarkerStyle::Bracket {
                bridge_glued_labels(text, &mut before, &mut chain, &mut score);
                chain.retain(|marker| marker.start < list);
                let held = candidates.iter().map(|marker| marker.start).collect::<HashSet<_>>();
                candidates.extend(before.into_iter().filter(|marker| !held.contains(&marker.start)));
            }
            let counted = candidates.iter().filter(|marker| marker.start < list).cloned().collect::<Vec<_>>();
            pooled.extend(counted.iter().cloned());
            extend_over_gaps(&mut chain, &counted);
            lost_root(&mut chain, &mut score, &counted);
            if style == MarkerStyle::Bare {
                rooted_on_author(text, &mut chain);
            }
            // Scored on the count it holds, not on where its first link ended: a count carried past a lost "[2]" is
            // as long as it reads.
            score = chain_score(&chain);
            // A decision of one paragraph numbers it "[1]", and no note call names it.
            let lone = style == MarkerStyle::Bracket && chain.len() == 1 && chain[0].number == 1 && candidates.len() == 1
                && !text.value[..text.byte(chain[0].start)].contains("[1]");
            if chain.len() < 2 && !lone || endnote_shaped(text, &chain) {
                continue;
            }
            let short = chain.len() < 5;
            if !short || (style == MarkerStyle::Bracket
                // A short ruling numbers its paragraphs "1." to "4." from the head of its reasons, no running prose before
                // them; after prose, such a count is a list.
                || style == MarkerStyle::Dot && !text.value[..text.byte(chain[0].start)].lines().any(running_prose)
                // A short ruling's first number stands on its judge: "1 CORY J. — We are all of the view ...". No list opens so.
                || style == MarkerStyle::Bare && text.value[text.byte(chain[0].content_start)..].lines().next().is_some_and(author_line))
                // A note list after the paragraphs counts from 1 again; the count stands alone before it.
                && sole_chain(&chain, &counted)
            {
                hypotheses.push(Hypothesis { style, markers: chain, all: candidates.into(), short, score });
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
            let short = scope.len() < 5;
            if !short || style == MarkerStyle::Bracket
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
                hypotheses.push(Hypothesis { style, markers: scope.clone(), all: Arc::clone(&all), short, score: 0.0 });
            }
        }
    }
    // A decision's numbering may change its style where another judge's reasons begin ("121." and then "122"): the
    // count reads across its styles where that carries it well past the count of any one of them.
    if rooted {
        pooled.sort_by_key(|marker| marker.start);
        let (mut chain, _) = rooted_chain(&pooled);
        extend_over_gaps(&mut chain, &pooled);
        let longest = hypotheses.iter().map(|value| value.markers.len()).max().unwrap_or(0);
        if chain.len() >= longest + 5 && chain.iter().any(|marker| marker.style != chain[0].style) {
            let score = chain_score(&chain);
            hypotheses.push(Hypothesis { style: chain[0].style, markers: chain, all: pooled.into(), short: false, score });
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
    // A decision numbering its paragraphs in parentheses ("(12) The SADI Program objectives …") may also hold
    // another short count, a list of its issues ("1. Was the Minister correct …"): its parenthesized count,
    // far the longer, is its paragraphs.
    let parenthesized = if rooted { parenthesized_paragraphs(text, excluded) } else { Vec::new() };
    let starts = parenthesized.iter().filter(|block| block.kind == NodeKind::Paragraph).map(|block| text.utf16(block.range.start)).collect::<Vec<_>>();
    let numbered = starts.len();
    // Spread through the decision, not cornered as a quoted enactment's subsections are; and never in place
    // of a bracketed count, which a decision quoting an enactment keeps.
    let spread = starts.first().zip(starts.last()).is_some_and(|(first, last)| (last - first) as f64 / text_length.max(1) as f64 >= 0.4);
    let outnumbered = |hypothesis: &Hypothesis| rooted && spread && hypothesis.style != MarkerStyle::Bracket
        && numbered >= 10 && numbered >= 2 * hypothesis.markers.len();
    for hypothesis in hypotheses {
        // A rooted count's paragraphs end at one another and at a second count from 1, not at the
        // numbers a paragraph quotes from another decision, which stay in the paragraph quoting them.
        // A rooted count's paragraph runs to the count's next number or another count's opening, not to
        // whatever number its text holds ("2007 CRTFP 84"); other readings measure to any candidate.
        let ends = if rooted {
            let mut ends = hypothesis.markers.iter().cloned().chain(restart_boundaries(text, &hypothesis.all, &hypothesis.markers)).collect::<Vec<_>>();
            ends.sort_by_key(|marker| marker.start);
            ends
        } else {
            hypothesis.all.to_vec()
        };
        let mut next = HashMap::with_capacity(ends.len());
        for pair in ends.windows(2) {
            next.insert(pair[0].start, pair[1].start);
        }
        if let Some(last) = ends.last() {
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
        let accepted = if hypothesis.short {
            text_length <= if rooted { 12_000 } else { 6_000 }
                // A provider's catalogue record opens its text, so a rooted count may start late.
                && (rooted || first_utf16 <= 1_200 || start <= 0.5)
                // A rooted count standing alone may be an endorsement of a few short paragraphs ("[1] The motion
                // judge did not err …", "[2] Accordingly, the appeal is dismissed …").
                && if rooted { counts.iter().sum::<usize>() >= 20 } else { counts.iter().copied().max().unwrap_or(0) >= 30 }
        } else {
            let substantive = counts.iter().filter(|value| **value >= 12).count() as f64
                / hypothesis.markers.len() as f64;
            (median >= 12.0 || mean >= 20.0 || maximum >= 30)
                && span >= 0.05
                && !(hypothesis.style == MarkerStyle::Bracket
                    && text_length > 6_000
                    && start > 0.7
                    && substantive < 0.5)
                && !(hypothesis.style != MarkerStyle::Bracket && substantive < 0.7)
                && !(hypothesis.style == MarkerStyle::Bare
                    && (median < 20.0 || span < 0.15 || start > 0.7))
        };
        if !accepted {
            continue;
        }
        if outnumbered(&hypothesis) {
            return parenthesized;
        }
        return paragraph_ranges(text, &hypothesis.markers, &ends, hypothesis.style, !strict, excluded);
    }
    if rooted {
        return parenthesized;
    }
    Vec::new()
}

pub(super) fn gapped_paragraphs(blocks: &[StructureNode]) -> bool {
    let mut prior = None;
    blocks
        .iter()
        .filter(|value| value.kind == NodeKind::Paragraph)
        .filter_map(|value| {
            value
                .label
                .as_deref()?
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
    mut blocks: Vec<StructureNode>,
    excluded: &[ScalarRange],
    text: &ScalarText<'_>,
) -> Vec<StructureNode> {
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
            // (A note's call inside the paragraph ends nothing: "... to NFCL[1] and provided".)
            if range.end < block.range.end && range.end - range.start <= 8 {
                continue;
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
