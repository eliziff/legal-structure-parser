//! Labels on each piece of the source, a line cut at every boundary of the frame's blocks: the
//! region of the unit holding it; whether it opens that unit, continues the unit open in its
//! stream, or resumes an earlier unit (pointed at); the unit a unit falls under (pointed at); and
//! whether a document opens or a paired note reference sits on it. Read alike from the gold
//! (training targets) and from the parser (tier 0), and decoded back into the gold's own form. The
//! gold's own labels decode to the gold: `selfcheck labels` must read 1.0 on every layer.
use crate::alignment::Alignment;
use crate::state::{block_id, Gold, State};
use legal_structure::{LayoutAnalysis, SourceSpan};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

pub const REGIONS: [&str; 17] = [
    "none", "text", "list_item", "heading", "quote", "note", "table", "contents", "caption", "headnote", "marginal_note",
    "furniture", "page_label", "line_label", "title", "note_quote", "provision",
];
pub const STARTS: [&str; 3] = ["inside", "unit", "resume"];

/// Characters [start, end) of one line of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Default, Debug)]
pub struct Label {
    pub region: usize,
    /// 0 continues the unit open in the piece's stream, 1 opens a unit, 2 resumes `continues`.
    pub start: usize,
    /// On a resuming piece, the first piece (line@offset) of the unit it resumes.
    pub continues: Option<String>,
    /// On a heading's first piece, its step from the heading before it in its document (`step`);
    /// 0 elsewhere. Parents are not labelled: they follow from regions and the levels and depths
    /// the steps rebuild by the hierarchy rule (`assemble`).
    pub level: usize,
    /// On a ranked body unit's first piece, its depth's step from the body unit before it in its
    /// document (`step`), as the scorer reads depths; 0 elsewhere.
    pub depth: usize,
    /// On a unit's first piece: the unit opens with an enumerator the gold marks.
    pub marker: bool,
    pub document: bool,
    pub reference: bool,
}

pub fn anchor(state: &State, piece: &Piece) -> String {
    format!("{}@{}", state.lines[piece.line].id, piece.start)
}

/// Levels and depths run 1 to `DEEPEST`; a step between two of them is one of `STEPS` classes.
pub const DEEPEST: usize = 9;
pub const STEPS: usize = 2 * DEEPEST + 1;

/// A heading's level (or a body unit's depth) as a step from the one before it in its document, as
/// the scorer reads them, the first stepping from 1: 10 plus the change (2 up eight, 10 the same,
/// 18 down eight). A heading read one level off at the top of a document then costs one step, not
/// every level below it.
pub fn step(previous: Option<usize>, level: usize) -> usize {
    DEEPEST + 1 + level.clamp(1, DEEPEST) - previous.unwrap_or(1)
}

/// The level (or depth) a step leads to from the one before it.
pub fn stepped(previous: Option<usize>, step: usize) -> usize {
    (previous.unwrap_or(1) + step.clamp(2, STEPS - 1)).saturating_sub(DEEPEST + 1).clamp(1, DEEPEST)
}

fn index_of(values: &[&str], value: &str) -> usize {
    values.iter().position(|v| *v == value).unwrap_or(0)
}

/// Units of one stream run on past the units of the others that interrupt them.
/// Regions that run as streams of their own: body, notes, marginal notes, furniture, page labels
/// and line labels.
const STREAMS: usize = 6;

fn stream(region: usize) -> usize {
    match REGIONS[region] {
        "note" | "note_quote" => 1,
        "marginal_note" => 2,
        "furniture" => 3,
        "page_label" => 4,
        "line_label" => 5,
        _ => 0,
    }
}

fn spans_of(state: &State, spans: &[SourceSpan], line_ids: &[String]) -> Vec<SourceSpan> {
    if !spans.is_empty() {
        return spans.to_vec();
    }
    line_ids
        .iter()
        .filter_map(|id| state.index.get(id).map(|&n| SourceSpan { line_id: id.clone(), start: 0, end: state.lines[n].text.len() }))
        .collect()
}

fn span_list(value: Option<&Value>) -> Vec<SourceSpan> {
    serde_json::from_value(value.cloned().unwrap_or(Value::Null)).unwrap_or_default()
}

/// Every line cut at every boundary of the frame's block spans, in its reading order.
pub fn pieces(state: &State) -> Vec<Piece> {
    let mut cuts: Vec<Vec<usize>> = state.lines.iter().map(|l| vec![0, l.text.len()]).collect();
    for group in &state.groups {
        for span in spans_of(state, &group.spans, &group.line_ids) {
            if let Some(&n) = state.index.get(&span.line_id) {
                let length = state.lines[n].text.len();
                cuts[n].extend([span.start.min(length), span.end.min(length)]);
            }
        }
    }
    let mut result = Vec::new();
    for (rank, &n) in state.reading_order.iter().enumerate() {
        let found = &mut cuts[n];
        found.sort_unstable();
        found.dedup();
        // A line with no characters is one empty piece: the gold types empty lines too.
        if found.len() == 1 {
            result.push((rank, Piece { line: n, start: 0, end: 0 }));
        }
        result.extend(found.windows(2).map(|w| (rank, Piece { line: n, start: w[0], end: w[1] })));
    }
    // Reading order at the span level where the state has one: columns sharing a source line
    // read column by column, as the gold typed them, not interleaved by offset.
    result.sort_by_key(|(rank, piece)| match state.span_rank.get(&(piece.line, piece.start)) {
        Some(&span) => (0, span, 0),
        None => (1, *rank, piece.start),
    });
    result.into_iter().map(|(_, piece)| piece).collect()
}

/// The piece holding a point of a line, by line.
pub struct Locator(HashMap<usize, Vec<(usize, usize, usize)>>);

impl Locator {
    pub fn new(pieces: &[Piece]) -> Self {
        let mut by_line: HashMap<usize, Vec<(usize, usize, usize)>> = HashMap::new();
        for (k, p) in pieces.iter().enumerate() {
            by_line.entry(p.line).or_default().push((p.start, p.end, k));
        }
        for found in by_line.values_mut() {
            found.sort_unstable();
        }
        Locator(by_line)
    }

    pub fn at(&self, line: usize, at: usize) -> Option<usize> {
        let found = self.0.get(&line)?;
        found.iter().find(|&&(a, b, _)| a <= at && at < b).or_else(|| found.iter().find(|&&(a, _, _)| a == at)).map(|&(_, _, k)| k)
    }

    fn within(&self, line: usize, start: usize, end: usize) -> impl Iterator<Item = usize> + '_ {
        self.0.get(&line).into_iter().flatten().filter(move |&&(a, b, _)| start <= a && b <= end).map(|&(_, _, k)| k)
    }
}

/// A line id or a line@offset anchor, as a line and an offset of `state`.
fn point(state: &State, id: &str) -> Option<(usize, usize)> {
    if let Some(&n) = state.index.get(id) {
        return Some((n, 0));
    }
    let (line, offset) = id.rsplit_once('@')?;
    Some((*state.index.get(line)?, offset.parse().ok()?))
}

fn find(parents: &mut [usize], x: usize) -> usize {
    let mut root = x;
    while parents[root] != root {
        root = parents[root];
    }
    let mut at = x;
    while parents[at] != root {
        let next = parents[at];
        parents[at] = root;
        at = next;
    }
    root
}

/// Each piece's labels, in the frame's reading order; regions are read at each unit's first piece.
pub fn labels(state: &State, region: &dyn Fn(&Piece) -> usize) -> Result<(Vec<Piece>, Vec<Label>), String> {
    let pieces = pieces(state);
    let locate = Locator::new(&pieces);
    let mut key_of: HashMap<String, usize> = HashMap::new();
    for (g, group) in state.groups.iter().enumerate() {
        key_of.entry(block_id(group)).or_insert(g);
    }
    // Each piece's block is the first holding it.
    let mut holder: Vec<Option<usize>> = vec![None; pieces.len()];
    for (g, group) in state.groups.iter().enumerate() {
        for span in spans_of(state, &group.spans, &group.line_ids) {
            let Some(&n) = state.index.get(&span.line_id) else { continue };
            for k in locate.within(n, span.start, span.end).collect::<Vec<_>>() {
                holder[k].get_or_insert(g);
            }
        }
    }
    // A unit is a block with the fragments its continuations join to it.
    let mut joined: Vec<usize> = (0..state.groups.len()).collect();
    let link = |a: &str, b: &str, joined: &mut Vec<usize>| {
        if let (Some(&a), Some(&b)) = (key_of.get(a), key_of.get(b)) {
            let (a, b) = (find(joined, a), find(joined, b));
            joined[a.max(b)] = a.min(b);
        }
    };
    for (kind, links) in &state.continuations {
        if kind == "table" {
            continue;
        }
        for (from, next) in links {
            link(from, &next.next, &mut joined);
        }
    }
    let mut annotations = state.annotations.clone();
    if !annotations.iter().any(|a| a.kind == "document") && !state.reading_order.is_empty() {
        let first = &state.lines[state.reading_order[0]];
        annotations.push(legal_structure::CompositionAnnotation {
            id: "source".into(), kind: "document".into(), line_ids: vec![first.id.clone()],
            spans: vec![SourceSpan { line_id: first.id.clone(), start: 0, end: first.text.len() }],
            attributes: serde_json::json!({"parent": null}).as_object().unwrap().clone(),
        });
    }
    let unit: Vec<Option<usize>> = holder.iter().map(|h| h.map(|g| find(&mut joined, g))).collect();
    let mut first: HashMap<usize, usize> = HashMap::new();
    for (k, u) in unit.iter().enumerate() {
        if let Some(u) = u {
            first.entry(*u).or_insert(k);
        }
    }
    let region_of: HashMap<usize, usize> = first.iter().map(|(&u, &k)| (u, region(&pieces[k]))).collect();
    let mut result = vec![Label::default(); pieces.len()];
    let mut open: [Option<usize>; STREAMS] = [None; STREAMS];
    for k in 0..pieces.len() {
        let Some(u) = unit[k] else { continue };
        let r = region_of[&u];
        if r == 0 {
            continue;
        }
        let s = stream(r);
        result[k].region = r;
        if first[&u] == k {
            result[k].start = 1;
        } else if open[s] != Some(u) {
            result[k].start = 2;
            result[k].continues = Some(anchor(state, &pieces[first[&u]]));
        }
        open[s] = Some(u);
    }
    // Markers: the unit's first block carries one.
    for (&u, &k) in &first {
        let _ = u;
        result[k].marker = holder[k].is_some_and(|g| state.groups[g].attributes.get("marker")
            .and_then(Value::as_array).is_some_and(|m| !m.is_empty()));
    }
    // Parents: the unit holding the block a unit's first block names.
    let mut parent_of: HashMap<usize, usize> = HashMap::new();
    for (&u, &k) in &first {
        let Some(name) = holder[k].and_then(|g| state.groups[g].attributes.get("parent")).and_then(Value::as_str) else { continue };
        let target = key_of.get(name).copied().or_else(|| point(state, name).and_then(|(n, at)| locate.at(n, at)).and_then(|k| holder[k]));
        let Some(p) = target.map(|g| find(&mut joined, g)).filter(|&p| p != u) else { continue };
        if first.contains_key(&p) {
            parent_of.insert(u, p);
        }
    }
    // Heading levels: the level a gold gives, else (a parse) one plus the heading ancestors.
    for (&u, &k) in &first {
        if REGIONS[region_of[&u]] != "heading" {
            continue;
        }
        if let Some(given) = holder[k].and_then(|g| state.groups[g].attributes.get("heading_level")).and_then(Value::as_u64) {
            result[k].level = (given as usize).clamp(1, DEEPEST);
            continue;
        }
        let (mut level, mut at, mut seen) = (1, parent_of.get(&u).copied(), HashSet::new());
        while let Some(p) = at.filter(|p| seen.insert(*p)) {
            level += usize::from(REGIONS[region_of[&p]] == "heading");
            at = parent_of.get(&p).copied();
        }
        result[k].level = level.min(DEEPEST);
    }
    // Body depths: the depth a gold gives, else (a parse) one plus the body ancestors, as the scorer
    // reads a tree.
    for (&u, &k) in &first {
        let region = REGIONS[region_of[&u]];
        if region == "heading" || !ranked(region) {
            continue;
        }
        if let Some(given) = holder[k].and_then(|g| state.groups[g].attributes.get("depth")).and_then(Value::as_u64) {
            result[k].depth = (given as usize).clamp(1, DEEPEST);
            continue;
        }
        let (mut depth, mut at, mut seen) = (1, parent_of.get(&u).copied(), HashSet::new());
        while let Some(p) = at.filter(|p| seen.insert(*p)) {
            depth += usize::from(REGIONS[region_of[&p]] != "heading");
            at = parent_of.get(&p).copied();
        }
        result[k].depth = depth.min(DEEPEST);
    }
    for a in &annotations {
        // A document under another opens on its first piece; the source's own needs no label.
        if a.kind == "document" && a.attributes.get("parent").is_some_and(|p| !p.is_null()) {
            let opening = a.spans.first().map(|s| (s.line_id.as_str(), s.start)).or(a.line_ids.first().map(|id| (id.as_str(), 0)));
            if let Some(k) = opening.and_then(|(id, at)| state.index.get(id).and_then(|&n| locate.at(n, at))) {
                result[k].document = true;
            }
        }
        if a.kind == "note" {
            for s in span_list(a.attributes.get("references")) {
                if let Some(k) = state.index.get(&s.line_id).and_then(|&n| locate.at(n, s.start)) {
                    result[k].reference = true;
                }
            }
        }
    }
    // Levels and depths as steps, in reading order, each document's first stepping from nothing.
    let (mut heading, mut body) = (None, None);
    for label in &mut result {
        if label.document {
            (heading, body) = (None, None);
        }
        if label.level > 0 {
            let level = label.level;
            label.level = step(heading, level);
            heading = Some(level);
        }
        if label.depth > 0 {
            let depth = label.depth;
            label.depth = step(body, depth);
            body = Some(depth);
        }
    }
    Ok((pieces, result))
}

/// Regions over spans of a frame's lines; a piece takes the region of the span holding it.
pub struct Regions(HashMap<usize, Vec<(usize, usize, usize)>>);

impl Regions {
    pub fn of(&self, piece: &Piece) -> usize {
        self.0.get(&piece.line)
            .and_then(|found| found.iter().find(|&&(a, b, _)| a <= piece.start && piece.end <= b && (a < b || piece.start == piece.end)))
            .map_or(0, |&(_, _, r)| r)
    }
}

/// The gold's regions: each unit's spans take the unit's region as scored (`state::regions`), the
/// line numbers cut from the lines they number (`state::line_labels`) theirs.
pub fn gold_regions(gold: &Gold, state: &State) -> Regions {
    let mut found: HashMap<usize, Vec<(usize, usize, usize)>> = HashMap::new();
    let cut = crate::state::line_labels(gold);
    for label in cut.iter().flat_map(|(labels, _)| labels) {
        if let Some(&n) = state.index.get(&label.line_id) {
            found.entry(n).or_default().push((label.start, label.end, index_of(&REGIONS, "line_label")));
        }
    }
    for (unit, scored) in gold.units.iter().zip(crate::state::regions(gold)) {
        if unit.lines.is_empty() { continue }
        let region = index_of(&REGIONS, scored);
        for &t in &unit.lines {
            for span in &cut[t].1 {
                if let Some(&n) = state.index.get(&span.line_id) {
                    found.entry(n).or_default().push((span.start, span.end, region));
                }
            }
        }
    }
    Regions(found)
}

/// The parser's regions, in the gold's terms: its block's role, a note or a table where an
/// annotation holds the block's span, a list item where the block is one.
pub fn parsed_regions(state: &State) -> Regions {
    let mut holders: HashMap<&str, Vec<(&str, usize, usize)>> = HashMap::new();
    for a in state.annotations.iter().filter(|a| a.kind == "note" || a.kind == "table") {
        for s in &a.spans {
            holders.entry(s.line_id.as_str()).or_default().push((a.kind.as_str(), s.start, s.end));
        }
        if a.spans.is_empty() {
            for id in &a.line_ids {
                holders.entry(id.as_str()).or_default().push((a.kind.as_str(), 0, usize::MAX));
            }
        }
    }
    let mut found: HashMap<usize, Vec<(usize, usize, usize)>> = HashMap::new();
    for group in &state.groups {
        let kind = match group.kind.as_str() { "prose" => "text", "header" | "footer" => "furniture", other => other };
        let list = kind == "text" && crate::state::parser_list_item(group);
        for span in spans_of(state, &group.spans, &group.line_ids) {
            let Some(&n) = state.index.get(&span.line_id) else { continue };
            let held = holders.get(span.line_id.as_str())
                .and_then(|all| all.iter().find(|(_, a, b)| *a <= span.start && span.start < *b).map(|(k, _, _)| *k));
            let region = match held {
                Some(kind) => kind,
                None if list => "list_item",
                None => kind,
            };
            found.entry(n).or_default().push((span.start, span.end, index_of(&REGIONS, region)));
        }
    }
    Regions(found)
}

/// The gold's labels carried onto another frame's pieces through the scorer's alignment: a piece
/// takes the label of the gold piece its first aligned character falls in, opening what that gold
/// piece opens only where it holds the gold piece's first character; pointers move to the piece
/// holding the start of the gold piece they name.
pub fn carry(reference: &State, gold_pieces: &[Piece], gold: &[Label], candidate: &State, pieces: &[Piece], alignment: &Alignment)
    -> Vec<Option<Label>> {
    let gold_at = Locator::new(gold_pieces);
    // Each candidate piece's aligned stretches of gold lines.
    let mut aligned: HashMap<usize, Vec<(usize, usize, usize)>> = HashMap::new();
    let mut stretches: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); pieces.len()];
    for (k, piece) in pieces.iter().enumerate() {
        for &(lo, hi, r, offset, limit) in &alignment.mappings[piece.line] {
            let (a, b) = (piece.start.max(lo), piece.end.min(hi));
            if a >= b {
                continue;
            }
            let exact = hi - lo == limit - offset;
            let (ga, gb) = if exact { (offset + a - lo, offset + b - lo) } else { (offset, limit) };
            stretches[k].push((r, ga, gb));
            aligned.entry(r).or_default().push((ga, gb, k));
        }
    }
    let moved = |name: &str| -> Option<String> {
        let (r, at) = point(reference, name)?;
        let k = aligned.get(&r)?.iter().find(|&&(a, b, _)| a <= at && at < b)?.2;
        Some(anchor(candidate, &pieces[k]))
    };
    (0..pieces.len()).map(|k| {
        let &(r, at, _) = stretches[k].first()?;
        let g = gold_at.at(r, at)?;
        let source = &gold[g];
        let mut label = Label { region: source.region, ..Label::default() };
        if at == gold_pieces[g].start {
            label.start = source.start;
            label.continues = source.continues.as_deref().and_then(moved);
            label.level = source.level;
            label.depth = source.depth;
            label.marker = source.marker;
            label.document = source.document;
        }
        // A paired reference anywhere in the gold this piece covers.
        label.reference = stretches[k].iter().any(|&(r, a, b)| {
            gold_at.0.get(&r).into_iter().flatten().any(|&(start, end, g)| start < b && a < end && gold[g].reference)
        });
        Some(label)
    }).collect()
}

/// The units the hierarchy orders (CONVENTIONS section 2); notes, marginal notes, contents and
/// furniture stay outside it.
const RANKED: [&str; 8] = ["text", "provision", "list_item", "heading", "quote", "table", "caption", "headnote"];
const WORD_OUTER: [&str; 7] = ["PART", "PARTIE", "BOOK", "LIVRE", "TITLE", "TITRE", "VOLUME"];
const WORD_INNER: [&str; 16] = ["ARTICLE", "CHAPTER", "CHAPITRE", "DIVISION", "SECTION", "SCHEDULE", "ANNEX", "ANNEXE",
    "APPENDIX", "TAB", "RULE", "RÈGLE", "REGLE", "CLAUSE", "FORM", "FORMULE"];
const ROMANS: [&str; 28] = ["i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x", "xi", "xii", "xiii", "xiv", "xv",
    "xvi", "xvii", "xviii", "xix", "xx", "xxi", "xxii", "xxiii", "xxiv", "xxv", "xxx", "xl", "l"];

struct Patterns {
    sec: Regex,
    dotted: Regex,
    num: Regex,
    roman: Regex,
    alpha: Regex,
    bullet: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        sec: Regex::new(r"^\d+(?:\.\d+)?\s*\(\d+\)").unwrap(),
        dotted: Regex::new(r"^(\d+(?:\.\d+)+)\.?").unwrap(),
        num: Regex::new(r"^[(\[]?\d+([.)\]:])?").unwrap(),
        roman: Regex::new(r"^\(?([IVXLCivxlc]{1,6})[.)\]:]").unwrap(),
        alpha: Regex::new(r"^\(?([A-Za-z]{1,2})[.)\]]").unwrap(),
        bullet: Regex::new(r"^[•·▪–—\-*●○■□►]\s").unwrap(),
    })
}

/// Whether `text` opens with one of `words` as a whole word, case apart.
fn opens_with_word(text: &str, words: &[&str]) -> bool {
    let upper: String = text.chars().take(12).collect::<String>().to_uppercase();
    words.iter().any(|w| upper.starts_with(w) && !upper[w.len()..].chars().next().is_some_and(char::is_alphanumeric))
}

/// Whether the token ending at `end` is followed by whitespace or the end of the text.
fn followed(text: &str, end: usize) -> bool {
    text[end..].chars().next().map_or(true, char::is_whitespace)
}

/// The lower-case letter a letter list item opens with, for reading "i" after "h" as a letter.
fn alpha_token(text: &str) -> Option<char> {
    let t = text.trim_start();
    let m = patterns().alpha.captures(t).filter(|m| followed(t, m.get(0).unwrap().end()))?;
    let mut letters = m[1].chars();
    match (letters.next(), letters.next()) {
        (Some(c), None) => c.to_lowercase().next(),
        _ => None,
    }
}

/// The numbering family a unit opens with ("" for none), hierarchy.py's `numbering`: `open_letter`
/// is the last letter of an open lower-case letter list, so "i" after "h" reads as a letter.
fn numbering(text: &str, open_letter: Option<char>) -> String {
    let t = text.trim_start();
    let p = patterns();
    if opens_with_word(t, &WORD_OUTER) {
        return "word_outer".into();
    }
    if opens_with_word(t, &WORD_INNER) {
        return "word_inner".into();
    }
    if p.sec.is_match(t) {
        return "sec".into();
    }
    if let Some(m) = p.dotted.captures(t).filter(|m| followed(t, m.get(0).unwrap().end())) {
        return format!("dec{}", (m[1].matches('.').count() + 1).min(4));
    }
    if let Some(m) = p.num.captures(t) {
        let end = m.get(0).unwrap().end();
        if m.get(1).map_or_else(|| t[end..].starts_with(char::is_whitespace), |_| followed(t, end)) {
            return "num".into();
        }
    }
    if let Some(m) = p.roman.captures(t).filter(|m| followed(t, m.get(0).unwrap().end())) {
        let token = &m[1];
        let lower = token.chars().all(char::is_lowercase);
        let low = token.to_lowercase();
        let (letter, numeral) = if lower { ("al", "rl") } else { ("AU", "RU") };
        if token.chars().count() == 1 && "ivxlc".contains(low.as_str()) {
            let first = low.chars().next().unwrap() as u32;
            if open_letter.is_some_and(|o| o as u32 + 1 == first) {
                return letter.into();
            }
            return numeral.into();
        }
        if ROMANS.contains(&low.as_str()) {
            return numeral.into();
        }
    }
    if let Some(m) = p.alpha.captures(t).filter(|m| followed(t, m.get(0).unwrap().end())) {
        let letters: Vec<char> = m[1].chars().collect();
        if letters.len() == 2 && letters[0] != letters[1] {
            return String::new();
        }
        return if letters.iter().all(|c| c.is_lowercase()) { "al" } else { "AU" }.into();
    }
    if p.bullet.is_match(t) {
        return "bullet".into();
    }
    String::new()
}

/// Parents of units in reading order from each unit's region, first-line text, heading level (0 off
/// headings) and document start: hierarchy.py's `assemble`, CONVENTIONS section 2. A heading falls
/// under the nearest earlier heading of a smaller level; text, tables, captions and headnotes
/// under the nearest earlier heading; a list item continues its numbering family's open list or
/// falls under the unit before it; a quote under the unit before it; a document start resets all.
pub fn ranked(region: &str) -> bool {
    RANKED.contains(&region)
}

/// A body unit with a depth (`depths`, 0 where none) sits inside the nearest earlier body unit of a
/// smaller depth under the same heading, else under the heading.
pub fn assemble(regions: &[&str], texts: &[String], levels: &[usize], depths: &[usize], starts_document: &[bool]) -> Vec<Option<usize>> {
    let mut parents = vec![None; regions.len()];
    let mut heading_stack: Vec<(usize, usize)> = Vec::new();
    let mut list_stack: Vec<(String, usize, Option<usize>)> = Vec::new();
    let mut body_stack: Vec<(usize, usize)> = Vec::new();
    let (mut prev, mut open_letter): (Option<usize>, Option<char>) = (None, None);
    for u in 0..regions.len() {
        let region = regions[u];
        if !RANKED.contains(&region) {
            continue;
        }
        if starts_document[u] {
            heading_stack.clear();
            list_stack.clear();
            body_stack.clear();
            prev = None;
            open_letter = None;
        }
        let top = heading_stack.last().map(|&(_, h)| h);
        if region != "heading" && depths[u] > 0 {
            while body_stack.last().is_some_and(|&(d, _)| d >= depths[u]) {
                body_stack.pop();
            }
            parents[u] = body_stack.last().map(|&(_, b)| b).or(top);
            body_stack.push((depths[u], u));
            prev = Some(u);
            continue;
        }
        if region == "heading" {
            body_stack.clear();
        }
        let parent = match region {
            "heading" => {
                let level = levels[u].max(1);
                while heading_stack.last().is_some_and(|&(l, _)| l >= level) {
                    heading_stack.pop();
                }
                let parent = heading_stack.last().map(|&(_, h)| h);
                heading_stack.push((level, u));
                list_stack.clear();
                open_letter = None;
                parent
            }
            "text" | "provision" | "table" | "caption" | "headnote" => {
                if matches!(region, "text" | "provision") {
                    list_stack.clear();
                    open_letter = None;
                }
                top
            }
            "list_item" => {
                let fam = numbering(&texts[u], open_letter);
                let found = if fam.is_empty() { None } else { list_stack.iter().rposition(|(f, _, _)| *f == fam) };
                let parent = if let Some(k) = found {
                    let parent = list_stack[k].2;
                    list_stack.truncate(k + 1);
                    list_stack[k] = (fam.clone(), u, parent);
                    parent
                } else if fam.is_empty() && !list_stack.is_empty() {
                    let last = list_stack.len() - 1;
                    let parent = list_stack[last].2;
                    list_stack[last].1 = u;
                    parent
                } else {
                    let parent = prev.or(top);
                    list_stack.push((fam.clone(), u, parent));
                    parent
                };
                if fam == "al" {
                    open_letter = alpha_token(&texts[u]);
                }
                parent
            }
            _ => match prev {
                Some(p) if regions[p] == "quote" => parents[p],
                other => other,
            },
        };
        parents[u] = parent;
        prev = Some(u);
    }
    parents
}

/// The enumerator a piece opens with ("12.", "[12]", "(a)", "1.2.3", "iv)"), as the gold marks a
/// unit's marker: its character range within the piece, when the engine's numbering grammar reads
/// the token. A bare number ("12 persons") is no marker.
fn leading_marker(text: &[char]) -> Option<(usize, usize)> {
    let n = text.len();
    let mut i = 0;
    while i < n && text[i].is_whitespace() {
        i += 1;
    }
    let start = i;
    let open = i < n && (text[i] == '(' || text[i] == '[');
    if open {
        i += 1;
    }
    let body = i;
    while i < n && (text[i].is_alphanumeric() || (text[i] == '.' && i + 1 < n && text[i + 1].is_alphanumeric())) {
        i += 1;
    }
    if i == body || i - body > 12 {
        return None;
    }
    if open {
        let close = if text[start] == '(' { ')' } else { ']' };
        if i < n && text[i] == close {
            i += 1;
        } else {
            return None;
        }
    } else if i < n && (text[i] == '.' || text[i] == ')') {
        i += 1;
    } else if !text[body..i].contains(&'.') {
        return None;
    }
    if i < n && !text[i].is_whitespace() {
        return None;
    }
    let token: String = text[start..i].iter().collect();
    legal_structure::STRUCTURE_ANALYSIS.numbering_marker(token.trim(), None).map(|_| (start, i))
}

/// Labels decoded into the gold's own form over a frame's pieces in its reading order. A piece that
/// opens a unit opens one; one that continues joins the unit open in its stream; one that resumes
/// joins the unit it names. The frame supplies what the labels do not say: markers, the paired
/// note references the labels keep, the notes they pair with, and table cells.
pub fn decode(candidate: &State, pieces: &[Piece], labels: &[Label], source_sha256: &str) -> Gold {
    use crate::state::{GoldLine, NotePair, Reference, Typed, Unit, V4};
    let locate = Locator::new(pieces);
    let piece_at = |span: &SourceSpan| candidate.index.get(&span.line_id).and_then(|&n| locate.at(n, span.start));
    // A block's marker, reachable from every piece the block holds: the decoded unit opens on
    // whichever of them reads first.
    // Likewise a block's transcript line numbers and speaker labels.
    let by_piece = |key: &str| {
        let mut found_at: HashMap<usize, Vec<SourceSpan>> = HashMap::new();
        for group in &candidate.groups {
            let found = span_list(group.attributes.get(key));
            if found.is_empty() {
                continue;
            }
            for span in spans_of(candidate, &group.spans, &group.line_ids) {
                let Some(&n) = candidate.index.get(&span.line_id) else { continue };
                for k in locate.within(n, span.start, span.end).collect::<Vec<_>>() {
                    found_at.entry(k).or_insert_with(|| found.clone());
                }
            }
        }
        found_at
    };
    let (markers, line_numbers, speakers) = (by_piece("marker"), by_piece("line_number"), by_piece("speaker"));
    let mut note_labels: HashMap<usize, Vec<SourceSpan>> = HashMap::new();
    let mut references: HashMap<usize, Vec<SourceSpan>> = HashMap::new();
    for a in candidate.annotations.iter().filter(|a| a.kind == "note") {
        let label = span_list(a.attributes.get("label"));
        if let (false, Some(k)) = (label.is_empty(), a.spans.first().and_then(piece_at)) {
            note_labels.entry(k).or_insert(label);
        }
        for s in span_list(a.attributes.get("references")) {
            if let Some(k) = piece_at(&s) {
                references.entry(k).or_default().push(s);
            }
        }
    }
    // Table cells by piece, each with its column and the width of its row.
    let mut cells: HashMap<usize, Vec<(usize, usize, SourceSpan)>> = HashMap::new();
    for a in candidate.annotations.iter().filter(|a| a.kind == "table") {
        for row in a.attributes.get("rows").and_then(Value::as_array).into_iter().flatten() {
            let row = row.as_array().cloned().unwrap_or_default();
            for (c, cell) in row.iter().enumerate() {
                for span in span_list(Some(cell)) {
                    if let Some(k) = piece_at(&span) {
                        cells.entry(k).or_default().push((c, row.len(), span));
                    }
                }
            }
        }
    }
    let lines: Vec<GoldLine> = candidate.lines.iter().map(|l| GoldLine {
        id: l.id.clone(), page: l.page, text: l.text.iter().collect(), corrected: None,
    }).collect();
    let mut typed: Vec<Typed> = Vec::new();
    let mut typed_of: HashMap<usize, usize> = HashMap::new();
    let mut units: Vec<Unit> = Vec::new();
    let mut unit_region: Vec<usize> = Vec::new();
    let mut head: Vec<usize> = Vec::new();
    let mut unit_at: HashMap<String, usize> = HashMap::new();
    let mut open: [Option<usize>; STREAMS] = [None; STREAMS];
    // The page each unit last took a piece on: a piece continues a unit of its region on its page or
    // the page before (the gold's own convention, hierarchy.py `units`); a piece the model reads as
    // continuing a unit left further back opens a unit of its own.
    let mut unit_page: Vec<usize> = Vec::new();
    for (k, piece) in pieces.iter().enumerate() {
        let label = &labels[k];
        if label.region == 0 {
            continue;
        }
        let s = stream(label.region);
        let page = candidate.lines[piece.line].page;
        let near = |u: usize| page <= unit_page[u] + 1;
        let resumed = if label.start == 2 { label.continues.as_deref().and_then(|a| unit_at.get(a).copied()) } else { None };
        let u = match (label.start, resumed, open[s]) {
            (2, Some(u), _) if near(u) => u,
            (0 | 2, _, Some(u)) if unit_region[u] == label.region && near(u) => u,
            _ => {
                units.push(Unit { lines: Vec::new(), parent: None, level: None, depth: None, starts_document: label.document, document_type: None });
                unit_region.push(label.region);
                unit_page.push(page);
                head.push(k);
                unit_at.insert(anchor(candidate, piece), units.len() - 1);
                units.len() - 1
            }
        };
        unit_page[u] = page;
        open[s] = Some(u);
        let region = REGIONS[unit_region[u]];
        let line = &candidate.lines[piece.line];
        let own = |s: &SourceSpan| piece_at(s) == Some(k);
        let read_marker = || {
            let chars = &line.text[piece.start.min(line.text.len())..piece.end.min(line.text.len())];
            leading_marker(chars).map(|(a, b)| vec![SourceSpan { line_id: line.id.clone(), start: piece.start + a, end: piece.start + b }])
        };
        let marker: Option<Vec<SourceSpan>> = if head[u] != k {
            None
        } else if region == "note" {
            note_labels.get(&k).cloned()
        } else {
            markers.get(&k).cloned().or_else(|| (label.marker && matches!(region, "text" | "provision" | "list_item")).then(read_marker).flatten())
        };
        let mut own_references: Vec<SourceSpan> = if label.reference {
            references.get(&k).into_iter().flatten().filter(|s| own(s)).cloned().collect()
        } else {
            Vec::new()
        };
        own_references.sort_by_key(|s| s.start);
        let row_cells = (region == "table").then(|| cells.get(&k)).flatten().map(|found| {
            let width = found.iter().map(|(c, w, _)| (*w).max(c + 1)).max().unwrap_or(0);
            let mut grid = vec![Vec::new(); width];
            for (c, _, span) in found {
                grid[*c].push(span.clone());
            }
            grid
        });
        typed_of.insert(k, typed.len());
        units[u].lines.push(typed.len());
        typed.push(Typed {
            region: region.to_owned(),
            text: String::new(),
            spans: vec![SourceSpan { line_id: line.id.clone(), start: piece.start, end: piece.end }],
            markers: marker.map(|m| vec![m]).unwrap_or_default(),
            references: own_references.into_iter().map(|s| Reference { spans: vec![s] }).collect(),
            cells: row_cells,
            line_number: if head[u] == k { line_numbers.get(&k).cloned().unwrap_or_default() } else { Vec::new() },
            speaker: if head[u] == k { speakers.get(&k).cloned().unwrap_or_default() } else { Vec::new() },
        });
    }
    let regions: Vec<&str> = unit_region.iter().map(|&r| REGIONS[r]).collect();
    // A unit's opening text for the numbering grammar: its first pieces joined, since a marker often
    // sits in a piece of its own ("1." | "The hearing ...", "•" | "Item").
    let texts: Vec<String> = units.iter().map(|unit| unit.lines.iter().take(3).map(|&t| {
        let span = &typed[t].spans[0];
        let line = &candidate.lines[candidate.index[&span.line_id]];
        line.text[span.start.min(line.text.len())..span.end.min(line.text.len())].iter().collect::<String>()
    }).collect::<Vec<_>>().join(" ")).collect();
    for (u, text) in texts.iter().enumerate() {
        typed[units[u].lines[0]].text = text.clone();
    }
    let starts: Vec<bool> = units.iter().map(|unit| unit.starts_document).collect();
    // Each heading's level and each body unit's depth from its step, each document's first stepping
    // from nothing.
    let (mut levels, mut depths) = (vec![0; units.len()], vec![0; units.len()]);
    let (mut heading, mut body) = (None, None);
    for u in 0..units.len() {
        if starts[u] {
            (heading, body) = (None, None);
        }
        if regions[u] == "heading" && labels[head[u]].level > 0 {
            levels[u] = stepped(heading, labels[head[u]].level);
            heading = Some(levels[u]);
            units[u].level = Some(levels[u]);
        } else if regions[u] != "heading" && ranked(regions[u]) && labels[head[u]].depth > 0 {
            depths[u] = stepped(body, labels[head[u]].depth);
            body = Some(depths[u]);
            units[u].depth = Some(depths[u]);
        }
    }
    for (u, parent) in assemble(&regions, &texts, &levels, &depths, &starts).into_iter().enumerate() {
        units[u].parent = parent;
    }
    if std::env::var("LEGALPDF_ASSEMBLE_TRACE").is_ok_and(|prefix| source_sha256.starts_with(&prefix)) {
        eprintln!("assemble-units {} pieces {} labelled {}", units.len(), pieces.len(), labels.iter().filter(|l| l.region != 0).count());
        for u in 0..units.len() {
            let piece = &pieces[head[u]];
            eprintln!("assemble {u} {} {} {} {:?} {:?} start={} doc={} head={}@{} marker_label={} block_marker={} typed_markers={}",
                regions[u], levels[u], numbering(&texts[u], None), units[u].parent, texts[u].chars().take(30).collect::<String>(),
                labels[head[u]].start, units[u].starts_document, candidate.lines[piece.line].id, piece.start, labels[head[u]].marker,
                markers.contains_key(&head[u]), typed[units[u].lines[0]].markers.len());
        }
    }
    // Note pairs: each frame note's references, to the unit holding the note's first piece.
    let mut notes: Vec<NotePair> = Vec::new();
    let unit_of_typed: HashMap<usize, usize> = units.iter().enumerate().flat_map(|(u, unit)| unit.lines.iter().map(move |&t| (t, u))).collect();
    for a in candidate.annotations.iter().filter(|a| a.kind == "note") {
        let Some(&t) = a.spans.first().and_then(piece_at).and_then(|k| typed_of.get(&k)) else { continue };
        let note = unit_of_typed[&t];
        if REGIONS[unit_region[note]] != "note" {
            continue;
        }
        for s in span_list(a.attributes.get("references")) {
            let Some(&t) = piece_at(&s).and_then(|k| typed_of.get(&k)) else { continue };
            if let Some(r) = typed[t].references.iter().position(|reference| reference.spans[0] == s) {
                notes.push(NotePair { line: t, reference: Some(r), note: Some(note) });
            }
        }
    }
    Gold { schema_version: V4.to_owned(), source_sha256: source_sha256.to_owned(), parser: Value::Null, lines, typed, units, notes }
}
