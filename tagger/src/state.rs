//! The scorer's frame: source lines, reading order, blocks over exact source spans, annotations
//! (notes, tables, documents) and continuations. The gold is projected into it from its typed
//! lines and units; a parser structure through the composition the apps read.
use legal_structure::{
    CompositionAnnotation, CompositionBlock, CompositionLine, CompositionLink, CompositionLinks,
    SourceSpan,
};
use legal_structure_model::DocumentStructure;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub struct Line {
    pub id: String,
    pub page: usize,
    /// The line's text as this side reads it: the gold's correction, or the parser's own.
    pub text: Vec<char>,
    /// (page, middle, left): the gold's reading position stands for its geometry.
    pub spatial: (usize, f64, f64),
    pub page_band: Option<f64>,
}

pub struct State {
    pub lines: Vec<Line>,
    pub index: HashMap<String, usize>,
    pub reading_order: Vec<usize>,
    /// Each typed span's reading rank by (line, start) when the state is a gold's: side-by-side
    /// columns share source lines, so reading order is finer than line order. A parser's state
    /// has none and reads in line order.
    pub span_rank: HashMap<(usize, usize), usize>,
    pub groups: Vec<CompositionBlock>,
    pub annotations: Vec<CompositionAnnotation>,
    pub continuations: CompositionLinks,
}

impl State {
    pub fn line(&self, id: &str) -> &Line {
        &self.lines[self.index[id]]
    }

    pub fn composition_lines(&self) -> Vec<CompositionLine> {
        self.lines
            .iter()
            .map(|line| CompositionLine {
                id: line.id.clone(),
                text: line.text.iter().collect(),
                page: line.page,
                evidence: BTreeMap::new(),
            })
            .collect()
    }

    pub fn order_ids(&self) -> Vec<String> {
        self.reading_order
            .iter()
            .map(|&n| self.lines[n].id.clone())
            .collect()
    }
}

pub fn anchor(span: &SourceSpan) -> String {
    format!("{}@{}", span.line_id, span.start)
}

/// A block's identity: its declared id, or its first span.
pub fn block_id(block: &CompositionBlock) -> String {
    if !block.id.is_empty() {
        return block.id.clone();
    }
    match block.spans.first() {
        Some(span) => anchor(span),
        None => format!("{}@0", block.line_ids[0]),
    }
}

fn unique_ids(spans: &[SourceSpan]) -> Vec<String> {
    let mut seen = HashSet::new();
    spans
        .iter()
        .filter(|s| seen.insert(s.line_id.clone()))
        .map(|s| s.line_id.clone())
        .collect()
}

fn link(next: String, separator: Option<&str>, kind: &str) -> CompositionLink {
    CompositionLink {
        next,
        separator: separator.map(str::to_owned),
        attributes: BTreeMap::from([("kind".to_owned(), json!(kind))]),
    }
}

fn attributes(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[derive(Deserialize)]
pub struct Gold {
    pub schema_version: String,
    pub source_sha256: String,
    #[serde(default)]
    pub parser: Value,
    pub lines: Vec<GoldLine>,
    pub typed: Vec<Typed>,
    pub units: Vec<Unit>,
    pub notes: Vec<NotePair>,
}

#[derive(Deserialize)]
pub struct GoldLine {
    pub id: String,
    pub page: usize,
    pub text: String,
    #[serde(default)]
    pub corrected: Option<String>,
}

#[derive(Deserialize)]
pub struct Typed {
    pub region: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub spans: Vec<SourceSpan>,
    #[serde(default)]
    pub markers: Vec<Vec<SourceSpan>>,
    #[serde(default)]
    pub references: Vec<Reference>,
    #[serde(default)]
    pub cells: Option<Vec<Vec<SourceSpan>>>,
}

#[derive(Deserialize)]
pub struct Reference {
    #[serde(default)]
    pub spans: Vec<SourceSpan>,
}

#[derive(Deserialize)]
pub struct Unit {
    pub lines: Vec<usize>,
    pub parent: Option<usize>,
    #[serde(default)]
    pub level: Option<usize>,
    #[serde(default)]
    pub starts_document: bool,
}

#[derive(Deserialize)]
pub struct NotePair {
    pub line: usize,
    pub reference: Option<usize>,
    pub note: Option<usize>,
}

/// The gold's convention for a numbered unit of the parser's: enumerated clauses and items
/// ("1.", "2.1", "(a)", "iv.", bullets) are list items; a bracketed paragraph number ("[12]"), a
/// bare statute section number ("12", before its "(1)") or a parenthesized subsection number
/// ("(2)", "(1.1)") opens running text.
pub fn parser_list_item(state: &State, block: &CompositionBlock) -> bool {
    let locator = block.attributes.get("locator_kind").and_then(Value::as_str);
    if locator == Some("list_item") {
        return true;
    }
    if !matches!(locator, Some("paragraph" | "section" | "clause" | "subclause")) {
        return false;
    }
    let spans: Vec<SourceSpan> =
        serde_json::from_value(block.attributes.get("marker").cloned().unwrap_or_default()).unwrap_or_default();
    let Some(first) = spans.first() else { return false };
    let Some(&n) = state.index.get(&first.line_id) else { return false };
    let chars = &state.lines[n].text;
    let text: String = chars[first.start.min(chars.len())..first.end.min(chars.len())].iter().collect();
    let marker = text.trim();
    let inner = marker.trim_start_matches(['(', '[']).trim_end_matches([')', ']']);
    let digits = !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit() || c == '.') && !inner.ends_with('.');
    let bare = digits && !inner.contains('.') && !marker.starts_with('(') && !marker.ends_with('.');
    let subsection = digits && marker.starts_with('(') && marker.ends_with(')') && inner.matches('.').count() <= 1;
    !(marker.is_empty() || marker.starts_with('[') || bare || subsection)
}

pub const V4: &str = "legal-structure-gold.v4";

/// The scorer's kinds for typed regions; the rest are running text (block quotes, notes and
/// tables inside their annotations, list items with a list_item locator).
fn kind_of(region: &str) -> &str {
    match region {
        "heading" | "caption" | "headnote" | "marginal_note" | "furniture" | "page_label"
        | "contents" => region,
        _ => "text",
    }
}

fn own(region: &str) -> bool {
    matches!(region, "furniture" | "page_label")
}

/// A v4 gold in the scorer's frame: one block per unit fragment (a unit's lines on one page,
/// unbroken), parents, markers, continuations, notes with their references, tables and documents.
/// The gold's reading order is the order the model typed its lines in.
pub fn projected(gold: &Gold) -> Result<State, String> {
    if gold.schema_version != V4 {
        return Err("Expected a legal-structure-gold.v4 file".into());
    }
    let index: HashMap<String, usize> = gold
        .lines
        .iter()
        .enumerate()
        .map(|(n, l)| (l.id.clone(), n))
        .collect();
    // Reading order: the typed order; a line never typed keeps its place after the typed line
    // before it in the extraction.
    let mut typed_order = Vec::new();
    let mut seen = HashSet::new();
    let mut span_rank: HashMap<(usize, usize), usize> = HashMap::new();
    for item in &gold.typed {
        for span in &item.spans {
            if seen.insert(span.line_id.as_str()) {
                typed_order.push(span.line_id.as_str());
            }
            if let Some(&n) = index.get(&span.line_id) {
                let rank = span_rank.len();
                span_rank.entry((n, span.start)).or_insert(rank);
            }
        }
    }
    let (mut leading, mut after, mut last) = (Vec::new(), HashMap::<&str, Vec<usize>>::new(), None);
    for (n, line) in gold.lines.iter().enumerate() {
        if seen.contains(line.id.as_str()) {
            last = Some(line.id.as_str());
        } else if let Some(previous) = last {
            after.entry(previous).or_default().push(n);
        } else {
            leading.push(n);
        }
    }
    let mut order = leading;
    for id in &typed_order {
        order.push(*index.get(*id).ok_or("Typed span names an absent line")?);
        order.extend(after.get(id).into_iter().flatten());
    }
    if order.len() != gold.lines.len() {
        return Err("Every gold line takes one reading position".into());
    }
    let mut position = vec![0usize; gold.lines.len()];
    for (k, &n) in order.iter().enumerate() {
        position[n] = k;
    }
    let lines: Vec<Line> = gold
        .lines
        .iter()
        .enumerate()
        .map(|(n, l)| Line {
            id: l.id.clone(),
            page: l.page,
            text: l.corrected.as_deref().unwrap_or(&l.text).chars().collect(),
            spatial: (l.page, position[n] as f64, 0.0),
            page_band: None,
        })
        .collect();
    let at = |span: &SourceSpan| (position[index[&span.line_id]], span.start);
    // Reading rank of a span: its typed rank, finer than its line's position when columns or a
    // marginal note share a line with the unit.
    let rank_key = |span: &SourceSpan| {
        let n = index[&span.line_id];
        match span_rank.get(&(n, span.start)) {
            Some(&rank) => (0, rank, 0),
            None => (1, position[n], span.start),
        }
    };
    let page_of = |span: &SourceSpan| gold.lines[index[&span.line_id]].page;
    let typed = &gold.typed;
    let units = &gold.units;
    // A unit's spans in reading order, split at each page and at each line of another unit that
    // comes between: what the source shows, never how the gold happened to group its typed spans.
    let mut placed: Vec<((usize, usize), usize, &SourceSpan)> = units
        .iter()
        .enumerate()
        .flat_map(|(u, unit)| unit.lines.iter().flat_map(move |&n| typed[n].spans.iter().map(move |s| (u, s))))
        .map(|(u, s)| (at(s), u, s))
        .collect();
    placed.sort_by_key(|&(_, u, span)| (rank_key(span), u));
    // Lines some unit holds text on, counted up to each reading position.
    let mut held = vec![false; gold.lines.len()];
    for &((line, _), _, _) in &placed {
        held[line] = true;
    }
    let mut before = vec![0usize; gold.lines.len() + 1];
    for (p, &h) in held.iter().enumerate() {
        before[p + 1] = before[p] + usize::from(h);
    }
    let mut parts: Vec<Vec<Vec<SourceSpan>>> = vec![Vec::new(); units.len()];
    let mut last: Vec<Option<(usize, &SourceSpan)>> = vec![None; units.len()];
    for &((line, _), u, span) in &placed {
        let split = match last[u] {
            None => true,
            // A part's spans run forward in line position: the composition engine orders a block's
            // spans by position and names it by the first, so a unit that reads backwards through
            // the extraction (a second column, a marginal note typed first) takes a new part there.
            Some((previous, prior)) => {
                page_of(prior) != page_of(span) || line > previous + 1 && before[line] > before[previous + 1]
                    || (line, span.start) < (previous, prior.start)
            }
        };
        if split {
            parts[u].push(Vec::new());
        }
        parts[u].last_mut().unwrap().push(span.clone());
        last[u] = Some((line, span));
    }
    let region: Vec<&str> = units
        .iter()
        .map(|u| typed[u.lines[0]].region.as_str())
        .collect();
    // The gold's parents are the hierarchy rule's own (CONVENTIONS section 2). A gold written
    // before the rule, or by another one, is refused here rather than rescored or converted.
    {
        let texts: Vec<String> = units.iter().map(|u| typed[u.lines[0]].text.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
        let levels: Vec<usize> = units.iter().map(|u| u.level.unwrap_or(0)).collect();
        let starts: Vec<bool> = units.iter().map(|u| u.starts_document).collect();
        let expected = crate::labels::assemble(&region, &texts, &levels, &starts);
        let differing = units.iter().zip(&expected).filter(|(u, e)| crate::labels::ranked(typed[u.lines[0]].region.as_str()) && u.parent != **e).count();
        if differing > 0 {
            return Err(format!("{differing} unit parents do not follow the hierarchy rule: the gold needs author.py's hierarchy pass"));
        }
    }
    let heads: Vec<Option<String>> = parts
        .iter()
        .map(|found| found.first().map(|spans| anchor(&spans[0])))
        .collect();
    // The scored hierarchy is the rule over the units the source shows: a typed unit without a
    // span (a line the OCR layer never produced) cannot be named by any parser, so the units
    // under it fall where the rule puts them without it.
    let shown: Vec<usize> = (0..units.len()).filter(|&u| heads[u].is_some()).collect();
    let scored_parent: Vec<Option<usize>> = {
        let regions: Vec<&str> = shown.iter().map(|&u| region[u]).collect();
        // The opening text as the source shows it (the decoder reads the same): the first three
        // spans of the unit's typed lines, joined.
        let texts: Vec<String> = shown.iter().map(|&u| units[u].lines.iter().flat_map(|&t| typed[t].spans.iter()).take(3).map(|span| {
            let text = &lines[index[&span.line_id]].text;
            text[span.start.min(text.len())..span.end.min(text.len())].iter().collect::<String>()
        }).collect::<Vec<_>>().join(" ")).collect();
        let levels: Vec<usize> = shown.iter().map(|&u| units[u].level.unwrap_or(0)).collect();
        let starts: Vec<bool> = shown.iter().map(|&u| units[u].starts_document).collect();
        let parents = crate::labels::assemble(&regions, &texts, &levels, &starts);
        let mut result = vec![None; units.len()];
        for (i, &u) in shown.iter().enumerate() {
            result[u] = parents[i].map(|p| shown[p]);
        }
        result
    };
    let mut groups = Vec::new();
    let mut annotations = Vec::new();
    let mut continuations = CompositionLinks::new();
    let mut notes: HashMap<usize, usize> = HashMap::new();
    let trace = std::env::var("LEGALPDF_ASSEMBLE_TRACE").is_ok_and(|p| gold.source_sha256.starts_with(&p));
    for (u, unit) in units.iter().enumerate() {
        let first = &typed[unit.lines[0]];
        let marker: Vec<SourceSpan> = first.markers.iter().flatten().cloned().collect();
        if trace {
            eprintln!("projected u{u} {} parts={} heads={:?} marker={}", region[u], parts[u].len(),
                parts[u].iter().map(|p| anchor(&p[0])).collect::<Vec<_>>(), marker.first().map_or(String::new(), anchor));
        }
        let parent = scored_parent[u].and_then(|p| heads[p].clone());
        for (k, spans) in parts[u].iter().enumerate() {
            let mut attrs = Map::new();
            attrs.insert("region".into(), json!(region[u]));
            attrs.insert(
                "parent".into(),
                parent.clone().map_or(Value::Null, Value::String),
            );
            if region[u] == "list_item" {
                attrs.insert("locator_kind".into(), json!("list_item"));
                attrs.insert(
                    "marker".into(),
                    json!(if k == 0 { marker.clone() } else { Vec::new() }),
                );
            } else if k == 0 && !marker.is_empty() && matches!(region[u], "text" | "quote") {
                attrs.insert("locator_kind".into(), json!("paragraph"));
                attrs.insert("marker".into(), json!(marker));
            } else if k == 0 && !marker.is_empty() && region[u] == "heading" {
                attrs.insert("marker".into(), json!(marker));
            }
            groups.push(CompositionBlock {
                id: String::new(),
                kind: kind_of(region[u]).to_owned(),
                line_ids: unique_ids(spans),
                spans: spans.clone(),
                attributes: attrs,
                document: None,
            });
            if region[u] == "note" {
                let label = if k == 0 {
                    first.markers.first().cloned().unwrap_or_default()
                } else {
                    Vec::new()
                };
                annotations.push(CompositionAnnotation {
                    id: format!("note:{}", anchor(&spans[0])),
                    kind: "note".into(),
                    line_ids: unique_ids(spans),
                    spans: spans.clone(),
                    attributes: attributes(
                        json!({"kind": "footnote", "label": label, "references": []}),
                    ),
                });
                if k == 0 {
                    notes.insert(u, annotations.len() - 1);
                }
            }
        }
        if own(region[u]) {
            continue;
        }
        let kind = match region[u] {
            "heading" => "heading",
            "note" => "note",
            _ => "paragraph",
        };
        for pair in parts[u].windows(2) {
            let (left, right) = (anchor(&pair[0][0]), anchor(&pair[1][0]));
            continuations
                .entry(kind.into())
                .or_default()
                .insert(left.clone(), link(right.clone(), Some(" "), kind));
            if region[u] == "list_item" {
                continuations
                    .entry("list".into())
                    .or_default()
                    .insert(left, link(right, None, "list"));
            }
        }
    }
    // Documents: the first block opens the source's own; each unit holding a document start opens
    // another under it.
    let opened: Vec<usize> = (0..units.len())
        .filter(|&u| !parts[u].is_empty() && units[u].starts_document)
        .collect();
    let start = parts
        .iter()
        .filter_map(|found| found.first().map(|spans| &spans[0]))
        .min_by_key(|s| at(s));
    if let (false, Some(start)) = (opened.is_empty(), start) {
        let root = format!("document:{}", anchor(start));
        let mut openings = vec![start.clone()];
        openings.extend(
            opened
                .iter()
                .map(|&u| parts[u][0][0].clone())
                .filter(|s| s != start),
        );
        for opening in openings {
            let parent = if &opening == start {
                Value::Null
            } else {
                json!(root)
            };
            annotations.push(CompositionAnnotation {
                id: format!("document:{}", anchor(&opening)), kind: "document".into(), line_ids: vec![opening.line_id.clone()],
                spans: vec![opening],
                attributes: attributes(json!({"kind": null, "parent": parent, "relationship": null, "title": [], "facets": {}})),
            });
        }
    }
    // Each note reference with the note the notes pass paired it with.
    for pair in &gold.notes {
        let (Some(note), Some(reference)) = (pair.note, pair.reference) else {
            continue;
        };
        let Some(&annotation) = notes.get(&note) else {
            continue;
        };
        let spans = &typed[pair.line].references[reference].spans;
        let references = annotations[annotation]
            .attributes
            .get_mut("references")
            .unwrap()
            .as_array_mut()
            .unwrap();
        references.extend(spans.iter().map(|s| json!(s)));
    }
    // Tables: table rows consecutive in reading order, with only furniture, page labels and notes
    // between, form one table, one grid per page linked across pages; a row is on the page of its
    // first cell. A unit without text stands nowhere and separates nothing.
    let mut tables: Vec<Vec<usize>> = Vec::new();
    let mut current = false;
    let mut in_order: Vec<usize> = (0..units.len()).filter(|&u| !parts[u].is_empty()).collect();
    in_order.sort_by_key(|&u| (rank_key(&parts[u][0][0]), u));
    for u in in_order {
        if region[u] == "table" {
            if !current {
                tables.push(Vec::new());
                current = true;
            }
            tables.last_mut().unwrap().push(u);
        } else if !own(region[u]) && region[u] != "note" {
            current = false;
        }
    }
    for rows in tables {
        let mut pages: Vec<(usize, Vec<Vec<Vec<SourceSpan>>>)> = Vec::new();
        for u in rows {
            let mut cells: BTreeMap<usize, Vec<SourceSpan>> = BTreeMap::new();
            // A typed line of the row with one cell where the row's first line had several is a
            // value wrapped onto the next line: it goes on in the row's last column, not column 0.
            let columns = units[u].lines.first().and_then(|&n| typed[n].cells.as_ref()).map_or(0, Vec::len);
            for &n in &units[u].lines {
                let line_cells = typed[n].cells.iter().flatten().collect::<Vec<_>>();
                let wrapped = columns >= 2 && line_cells.len() == 1;
                for (c, spans) in line_cells.into_iter().enumerate() {
                    let column = if wrapped { columns - 1 } else { c };
                    cells.entry(column).or_default().extend(spans.iter().cloned());
                }
            }
            // A cell's text in reading order.
            let width = cells.keys().next_back().map_or(0, |c| c + 1);
            let row: Vec<Vec<SourceSpan>> = (0..width)
                .map(|c| {
                    let mut spans = cells.remove(&c).unwrap_or_default();
                    spans.sort_by_key(|s| at(s));
                    spans
                })
                .collect();
            let Some(first) = row.iter().flatten().min_by_key(|s| at(s)) else {
                continue;
            };
            let page = page_of(first);
            match pages.iter_mut().find(|(p, _)| *p == page) {
                Some((_, grid)) => grid.push(row),
                None => pages.push((page, vec![row])),
            }
        }
        if pages.is_empty() {
            continue;
        }
        let width = pages
            .iter()
            .flat_map(|(_, grid)| grid.iter().map(Vec::len))
            .max()
            .unwrap_or(0);
        let mut chain = Vec::new();
        for (_, grid) in pages {
            let grid: Vec<Vec<Vec<SourceSpan>>> = grid
                .into_iter()
                .map(|mut row| {
                    row.resize(width, Vec::new());
                    row
                })
                .collect();
            let populated: Vec<SourceSpan> = grid.iter().flatten().flatten().cloned().collect();
            let first = anchor(&populated[0]);
            annotations.push(CompositionAnnotation {
                id: format!("table:{first}"),
                kind: "table".into(),
                line_ids: unique_ids(&populated),
                spans: populated,
                attributes: attributes(json!({"rows": grid, "header_rows": [], "merges": []})),
            });
            chain.push(first);
        }
        for pair in chain.windows(2) {
            continuations
                .entry("table".into())
                .or_default()
                .insert(pair[0].clone(), link(pair[1].clone(), None, "table"));
        }
    }
    Ok(State {
        lines,
        index,
        reading_order: order,
        span_rank,
        groups,
        annotations,
        continuations,
    })
}

/// One page of the parser's extraction, as the export carries it: its size and its lines' boxes.
pub struct ExtractionPage {
    pub width: f64,
    pub height: f64,
    pub lines: Vec<(String, [f64; 4])>,
}

/// A parser structure in the scorer's frame, through the same composition the apps read.
pub fn parsed(
    structure: &DocumentStructure,
    extraction: &[ExtractionPage],
) -> Result<State, String> {
    if structure.offset_unit != "utf16" {
        return Err("Expected the native UTF-16 structure contract".into());
    }
    let units: Vec<u16> = structure.text.encode_utf16().collect();
    let slice = |start: usize, end: usize| -> Result<String, String> {
        String::from_utf16(
            units
                .get(start..end)
                .ok_or("Invalid native source fragment")?,
        )
        .map_err(|e| e.to_string())
    };
    let mut sources: Vec<(String, String, usize, usize, usize)> = Vec::new();
    if !structure.source_fragments.is_empty() {
        for fragment in &structure.source_fragments {
            let (start, end) = (fragment.range.start, fragment.range.end);
            sources.push((
                fragment.id.clone(),
                slice(start, end)?,
                fragment.page_index + 1,
                start,
                end,
            ));
        }
    } else {
        for page in structure
            .nodes
            .iter()
            .filter(|n| n.kind == legal_structure_model::NodeKind::Page)
        {
            let (mut start, end) = (page.range.start, page.range.end);
            let content = slice(start, end)?;
            if page.line_ids.is_empty() && content.split('\n').next().unwrap_or("").is_empty() {
                continue;
            }
            let pieces: Vec<&str> = content.split('\n').collect();
            if pieces.len() != page.line_ids.len() {
                return Err("Native page source IDs do not match its text range".into());
            }
            for (id, text) in page.line_ids.iter().zip(pieces) {
                let stop = start + text.encode_utf16().count();
                sources.push((
                    id.clone(),
                    text.to_owned(),
                    page.page_indexes[0] + 1,
                    start,
                    stop,
                ));
                start = stop + 1;
            }
        }
    }
    let mut boxes: HashMap<&str, ([f64; 4], f64)> = HashMap::new();
    for page in extraction {
        for (id, bbox) in &page.lines {
            boxes.insert(
                id.as_str(),
                (
                    [
                        1000.0 * bbox[0] / page.width,
                        1000.0 * bbox[1] / page.height,
                        1000.0 * bbox[2] / page.width,
                        1000.0 * bbox[3] / page.height,
                    ],
                    (bbox[1] + bbox[3]) / 2.0 / page.height,
                ),
            );
        }
    }
    if sources
        .iter()
        .map(|s| s.0.as_str())
        .collect::<HashSet<_>>()
        .len()
        != sources.len()
    {
        return Err("Duplicate native line ID".into());
    }
    let mut lines = Vec::new();
    let mut composition = Vec::new();
    for (id, text, page, start, end) in &sources {
        // A line the extraction does not place has no box and no band.
        let (bbox, band) = boxes
            .get(id.as_str())
            .map_or(([0.0; 4], None), |(bbox, band)| (*bbox, Some(*band)));
        lines.push(Line {
            id: id.clone(),
            page: *page,
            text: text.chars().collect(),
            spatial: (*page, (bbox[1] + bbox[3]) / 2.0, bbox[0]),
            page_band: band,
        });
        composition.push(CompositionLine {
            id: id.clone(),
            text: text.clone(),
            page: *page,
            evidence: BTreeMap::from([("range".to_owned(), json!({"start": start, "end": end}))]),
        });
    }
    let prepared = legal_structure::structure_composition_blocks(
        legal_structure::StructureCompositionBlocksRequest {
            structure: structure.clone(),
            lines: composition,
        },
    )?;
    let index: HashMap<String, usize> = lines
        .iter()
        .enumerate()
        .map(|(n, l)| (l.id.clone(), n))
        .collect();
    let mut groups = prepared.groups;
    // A tagger's node states its region; the parser-output remap of marked text to list items
    // (parser_list_item) is for the parser's own nodes.
    let tagged: HashMap<&str, &str> = structure.nodes.iter()
        .filter(|n| n.origin_id == crate::graph::ORIGIN)
        .filter_map(|n| crate::graph::region_of(n).map(|region| (n.id.as_str(), region)))
        .collect();
    for group in &mut groups {
        if let Some(region) = group.attributes.get("native_id").and_then(Value::as_str).and_then(|id| tagged.get(id)) {
            group.attributes.insert("region".into(), json!(region));
        }
    }
    // The structure leaves running heads, folios and print margins out of every node, and names
    // each page's folio line; such a line in the page's outer bands is furniture.
    let folios: HashSet<&str> = structure
        .nodes
        .iter()
        .filter(|n| n.kind == legal_structure_model::NodeKind::Page)
        .filter_map(|n| n.anchor.as_deref())
        .collect();
    for group in &mut groups {
        if !matches!(group.kind.as_str(), "prose" | "text")
            || group.attributes.contains_key("native_id")
        {
            continue;
        }
        if group.line_ids.iter().any(|id| folios.contains(id.as_str())) {
            group.kind = "page_label".into();
        } else if !group.line_ids.is_empty() {
            let bands: Option<Vec<f64>> = group
                .line_ids
                .iter()
                .map(|id| lines[index[id]].page_band)
                .collect();
            if let Some(bands) = bands {
                let middle = bands.iter().sum::<f64>() / bands.len() as f64;
                if !(0.15..=0.85).contains(&middle) {
                    group.kind = "furniture".into();
                }
            }
        }
    }
    // A PDF parser's constituents are sections under the package's own document.
    let mut annotations = prepared.annotations;
    let roots: Vec<CompositionAnnotation> = annotations
        .iter()
        .filter(|a| a.kind == "document")
        .cloned()
        .collect();
    annotations.retain(|a| a.kind != "document");
    if let Some(root) = roots.into_iter().next() {
        let first = root.line_ids[0].clone();
        annotations.push(root);
        for node in structure.nodes.iter().filter(|n| {
            n.kind == legal_structure_model::NodeKind::Section
                && n.grammar
                    .as_deref()
                    .is_some_and(|g| g.starts_with("constituent_"))
                && !n.line_ids.is_empty()
        }) {
            let kind = &node.grammar.as_deref().unwrap()["constituent_".len()..];
            let relationship = match kind {
                "exhibit" => "exhibit",
                "schedule" => "schedule",
                "annex" => "attachment",
                _ => "component",
            };
            annotations.push(CompositionAnnotation {
                id: node.id.clone(), kind: "document".into(), line_ids: node.line_ids.clone(), spans: Vec::new(),
                attributes: attributes(json!({"kind": kind, "parent": first, "relationship": relationship, "title": [], "facets": {}})),
            });
        }
    }
    let reading_order = (0..lines.len()).collect();
    Ok(State {
        lines,
        index,
        reading_order,
        span_rank: HashMap::new(),
        groups,
        annotations,
        continuations: prepared.continuations,
    })
}
