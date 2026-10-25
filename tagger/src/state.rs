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
    /// A transcript line number printed on the line it numbers, which the extraction read into it.
    #[serde(default)]
    pub line_number: Vec<SourceSpan>,
    /// The label that opens a speaker's turn on this line ("Q.", "MR. SMITH:").
    #[serde(default)]
    pub speaker: Vec<SourceSpan>,
}

#[derive(Deserialize)]
pub struct Reference {
    #[serde(default)]
    pub spans: Vec<SourceSpan>,
}

#[derive(Deserialize)]
pub struct Unit {
    pub lines: Vec<usize>,
    /// The tagger's decoded tree (`labels::decode`), which its graph is built on; a gold has none
    /// and the scorer reads none.
    #[serde(default)]
    pub parent: Option<usize>,
    /// A heading's level, as the model gave it: 1 the outermost in its document.
    #[serde(default)]
    pub level: Option<usize>,
    /// A body unit's depth, as the model gave it: 1 directly under the headings, 2 inside a unit
    /// of depth 1; a unit sits inside the nearest unit before it of a smaller depth.
    #[serde(default)]
    pub depth: Option<usize>,
    #[serde(default)]
    pub starts_document: bool,
    /// On a document's first unit, what the document is, as the document pass gave it.
    #[serde(default)]
    pub document_type: Option<String>,
}

/// The documents whose numbered body units are provisions.
const INSTRUMENTS: [&str; 2] = ["contract", "legislation"];

/// Each unit's region as scored: the region of its first typed line, except that in an authored
/// gold a body unit (text or list item) opening with a printed number in a document typed contract
/// or legislation is a provision, and nothing else is. The page pass is never asked which body
/// units are provisions; it follows from the document's type and the unit's number, which two
/// authorings agree on far more often than on that call. A gold with no document types (the
/// tagger's decoded one) keeps its regions as given.
pub fn regions(gold: &Gold) -> Vec<&str> {
    let typed = gold.units.iter().any(|u| u.document_type.is_some());
    let mut instrument = false;
    gold.units.iter().map(|unit| {
        if let Some(kind) = &unit.document_type {
            instrument = INSTRUMENTS.contains(&kind.as_str());
        }
        let region = gold.typed[unit.lines[0]].region.as_str();
        if !typed || !matches!(region, "text" | "list_item" | "provision") {
            return region;
        }
        let numbered = unit.lines.iter().any(|&n| !gold.typed[n].markers.is_empty());
        if instrument && numbered { "provision" } else if region == "provision" { "text" } else { region }
    }).collect()
}

#[derive(Deserialize)]
pub struct NotePair {
    pub line: usize,
    pub reference: Option<usize>,
    pub note: Option<usize>,
}

/// A numbered unit of the parser's is a list item when the parser located it as one; numbered
/// paragraphs, sections and clauses are not, however the number is written.
pub fn parser_list_item(block: &CompositionBlock) -> bool {
    block.attributes.get("locator_kind").and_then(Value::as_str) == Some("list_item")
}

pub const V4: &str = "legal-structure-gold.v4";

/// The scorer's kinds for typed regions; the rest are running text (block quotes, notes and
/// tables inside their annotations, list items with a list_item locator).
fn kind_of(region: &str) -> &str {
    match region {
        "title" | "heading" | "caption" | "headnote" | "marginal_note" | "furniture" | "page_label"
        | "line_label" | "contents" => region,
        // A block quotation inside a note is scored as note until the parser tells them apart.
        "note_quote" => "note",
        _ => "text",
    }
}

fn own(region: &str) -> bool {
    matches!(region, "furniture" | "page_label" | "line_label")
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
    let region = regions(gold);
    // The gold's hierarchy is the levels the model gave: a heading's level, and a body unit's
    // depth. The scorer reads them as given; nothing derives a tree.
    let mut groups = Vec::new();
    let mut annotations = Vec::new();
    let mut continuations = CompositionLinks::new();
    let mut notes: HashMap<usize, usize> = HashMap::new();
    let trace = std::env::var("LEGALPDF_ASSEMBLE_TRACE").is_ok_and(|p| gold.source_sha256.starts_with(&p));
    for (u, unit) in units.iter().enumerate() {
        let first = &typed[unit.lines[0]];
        // Every printed marker the unit opens with, on whichever of its lines ("6", then "(1) Every lot").
        let marker: Vec<SourceSpan> = unit.lines.iter().flat_map(|&n| typed[n].markers.iter().flatten()).cloned().collect();
        if trace {
            eprintln!("projected u{u} {} parts={} heads={:?} marker={}", region[u], parts[u].len(),
                parts[u].iter().map(|p| anchor(&p[0])).collect::<Vec<_>>(), marker.first().map_or(String::new(), anchor));
        }
        for (k, spans) in parts[u].iter().enumerate() {
            let mut attrs = Map::new();
            attrs.insert("region".into(), json!(region[u]));
            if let Some(level) = unit.level.filter(|_| region[u] == "heading") {
                attrs.insert("heading_level".into(), json!(level));
            }
            if let Some(depth) = unit.depth.filter(|_| region[u] != "heading") {
                attrs.insert("depth".into(), json!(depth));
            }
            if region[u] == "list_item" {
                attrs.insert("locator_kind".into(), json!("list_item"));
                attrs.insert(
                    "marker".into(),
                    json!(if k == 0 { marker.clone() } else { Vec::new() }),
                );
            } else if k == 0 && !marker.is_empty() && matches!(region[u], "text" | "provision" | "quote") {
                attrs.insert("locator_kind".into(), json!("paragraph"));
                attrs.insert("marker".into(), json!(marker));
            } else if k == 0 && !marker.is_empty() && region[u] == "heading" {
                attrs.insert("marker".into(), json!(marker));
            }
            // Line numbers and speaker labels the unit's lines print, on its first part.
            if k == 0 {
                for (key, found) in [("line_number", unit.lines.iter().flat_map(|&n| typed[n].line_number.iter()).cloned().collect::<Vec<_>>()),
                                     ("speaker", unit.lines.iter().flat_map(|&n| typed[n].speaker.iter()).cloned().collect())] {
                    if !found.is_empty() {
                        attrs.insert(key.into(), json!(found));
                    }
                }
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

/// A parser structure in the scorer's frame, through the same composition the apps read.
pub fn parsed(structure: &DocumentStructure) -> Result<State, String> {
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
    for (k, (id, text, page, start, end)) in sources.iter().enumerate() {
        // The parser's reading order is its text order, so the alignment's page-local diff follows it,
        // as it follows the gold's typed order on the other side; box geometry would interleave the
        // columns of a two-column page.
        lines.push(Line {
            id: id.clone(),
            page: *page,
            text: text.chars().collect(),
            spatial: (*page, k as f64, 0.0),
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
    // A tagger's node states its region; reading list items off the locator (parser_list_item) is
    // for the parser's own nodes.
    let tagged: HashMap<&str, &str> = structure.nodes.iter()
        .filter(|n| n.origin_id == crate::graph::ORIGIN)
        .filter_map(|n| crate::graph::region_of(n).map(|region| (n.id.as_str(), region)))
        .collect();
    for group in &mut groups {
        if let Some(region) = group.attributes.get("native_id").and_then(Value::as_str).and_then(|id| tagged.get(id)) {
            group.attributes.insert("region".into(), json!(region));
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
