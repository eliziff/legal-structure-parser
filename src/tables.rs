use super::*;
use crate::text::ScalarText;

#[cfg(target_pointer_width = "32")]
const MAX_SAFE_INTEGER: usize = usize::MAX;
#[cfg(not(target_pointer_width = "32"))]
const MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991;
const MAX_TABLE_CELLS: usize = 500_000;

struct Cell<'a> {
    fact: &'a AuthoritativeTableCell,
    range: ScalarRange,
    ranges: Vec<ScalarRange>,
    bytes: Vec<std::ops::Range<usize>>,
}

pub(crate) struct AuthoritativeTables<'a> {
    cells: Vec<Cell<'a>>,
}

impl<'a> AuthoritativeTables<'a> {
    pub(crate) fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn new(
        coordinates: &ScalarText<'_>,
        facts: &'a [AuthoritativeTableCell],
    ) -> Result<Self, EngineError> {
        if facts.is_empty() {
            return Ok(Self { cells: Vec::new() });
        }
        let mut cells = Vec::with_capacity(facts.len());
        let mut occupied = Vec::with_capacity(facts.len());
        let mut occupied_count = 0usize;
        let invalid = || EngineError::invalid("Invalid table-cell coordinates or text bounds");
        for (index, fact) in facts.iter().enumerate() {
            let span_end = |start: usize, span: Option<usize>| {
                start.checked_add(span.unwrap_or(1).checked_sub(1)?)
            };
            let Some((last_row, last_column)) =
                span_end(fact.row, fact.row_span).zip(span_end(fact.column, fact.column_span))
            else {
                return Err(invalid());
            };
            if ![fact.table, fact.row, fact.column, last_row, last_column]
                .into_iter()
                .all(|value| (1..=MAX_SAFE_INTEGER).contains(&value))
                || fact.end > MAX_SAFE_INTEGER
                || fact.end < fact.start
                || fact.end > coordinates.utf16_len()
            {
                return Err(invalid());
            }
            let area = (last_row - fact.row + 1)
                .checked_mul(last_column - fact.column + 1)
                .ok_or_else(invalid)?;
            occupied_count = occupied_count
                .checked_add(area)
                .filter(|count| *count <= MAX_TABLE_CELLS)
                .ok_or_else(invalid)?;
            occupied.reserve(area);
            occupied.extend((fact.row..=last_row).flat_map(|row| {
                (fact.column..=last_column).map(move |column| (fact.table, row, column, index))
            }));
            let mut ranges = fact.ranges.clone();
            ranges.sort_by_key(|range| (range.start, range.end));
            if ranges.is_empty() || ranges.first().unwrap().start != fact.start
                || ranges.iter().map(|range| range.end).max() != Some(fact.end)
                || ranges.iter().any(|range| range.start > range.end || range.end > fact.end)
                || ranges.windows(2).any(|pair| pair[0].end > pair[1].start)
            {
                return Err(invalid());
            }
            let bytes = ranges.iter().map(|range| {
                coordinates.byte_at_utf16(range.start).zip(coordinates.byte_at_utf16(range.end))
                    .map(|(start, end)| start..end)
                    .ok_or_else(|| EngineError::invalid("Table-cell text range splits a Unicode scalar"))
            }).collect::<Result<Vec<_>, _>>()?;
            cells.push(Cell {
                fact,
                range: ScalarRange { start: fact.start, end: fact.end },
                ranges,
                bytes,
            });
        }
        occupied.sort_unstable();
        if let Some(index) = occupied
            .chunk_by(|left, right| left.0 == right.0 && left.1 == right.1 && left.2 == right.2)
            .filter(|coordinates| coordinates.len() > 1)
            .map(|coordinates| coordinates[1].3)
            .min()
        {
            let fact = &facts[index];
            let kind = if facts[..index].iter().any(|previous| {
                (previous.table, previous.row, previous.column)
                    == (fact.table, fact.row, fact.column)
            }) {
                "Duplicate"
            } else {
                "Overlapping"
            };
            return Err(EngineError::invalid(format!(
                "{kind} table-cell address: table:{}/row:{}/col:{}",
                fact.table, fact.row, fact.column
            )));
        }
        cells.sort_unstable_by_key(|cell| {
            (
                cell.fact.table,
                cell.fact.row,
                cell.range.start,
                cell.fact.column,
            )
        });
        Ok(Self { cells })
    }

    pub fn masked_text(&self, text: String) -> String {
        if self.cells.is_empty() {
            return text;
        }
        if text.is_ascii() {
            let mut masked = text.into_bytes();
            for range in self.cells.iter().flat_map(|cell| &cell.bytes) {
                masked[range.clone()]
                    .iter_mut()
                    .filter(|byte| **byte != b'\n')
                    .for_each(|byte| *byte = b' ');
            }
            return String::from_utf8(masked).unwrap();
        }
        let mut ranges: Vec<_> = self.cells.iter().flat_map(|cell| cell.bytes.clone()).collect();
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        let mut masked = String::with_capacity(text.len());
        let mut at = 0;
        for range in ranges {
            let start = at.max(range.start);
            let end = start.max(range.end);
            masked.push_str(&text[at..start]);
            for character in text[start..end].chars() {
                if character == '\n' {
                    masked.push('\n');
                } else {
                    masked.push(' ');
                    if character.len_utf16() == 2 {
                        masked.push(' ');
                    }
                }
            }
            at = end;
        }
        masked.push_str(&text[at..]);
        masked
    }

    pub fn nodes(
        &self,
        semantic_nodes: &[StructureNode],
        origin_id: &'static str,
    ) -> Vec<StructureNode> {
        if self.cells.is_empty() {
            return Vec::new();
        }
        let depths = node_depths(semantic_nodes);
        let mut nodes = Vec::with_capacity(self.cells.len() * 2);
        let native = |id, kind, range, parent| {
            StructureNode::new(id, kind, range, origin_id, Derivation::Native, parent)
        };
        for cells in self
            .cells
            .chunk_by(|left, right| left.fact.table == right.fact.table)
        {
            let table_number = cells[0].fact.table;
            let range = covering_range(cells);
            let owner = semantic_nodes
                .iter()
                .rev()
                .filter(|node| {
                    node.kind == NodeKind::Section
                        && node.range.start <= range.start
                        && range.end <= node.range.end
                })
                .max_by_key(|node| depths[node.id.as_str()]);
            let table_id = format!("table:{table_number}");
            let table_name = cells
                .iter()
                .min_by_key(|cell| {
                    (
                        cell.fact.table_name.as_deref().is_none_or(str::is_empty),
                        cell.range.start,
                        cell.fact.row,
                        cell.fact.column,
                    )
                })
                .and_then(|cell| {
                    cell.fact
                        .table_name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                });
            let mut table = native(
                table_id.clone(),
                NodeKind::Table,
                range,
                owner.map(|node| node.id.clone()),
            );
            table.ranges = cells.iter().flat_map(|cell| cell.ranges.clone()).collect();
            table.table_number = Some(table_number);
            table.label = Some(table_id.clone());
            table.aliases = table_name.map(|name| vec![name.to_owned()]);
            nodes.push(table);
            for cells in cells.chunk_by(|left, right| left.fact.row == right.fact.row) {
                let row_number = cells[0].fact.row;
                let row_id = format!("{table_id}/row:{row_number}");
                let row_range = covering_range(&cells);
                let mut row = native(
                    row_id.clone(),
                    NodeKind::Row,
                    row_range,
                    Some(table_id.clone()),
                );
                row.ranges = cells.iter().flat_map(|cell| cell.ranges.clone()).collect();
                row.row = Some(row_number);
                row.label = Some(row_id.clone());
                nodes.push(row);
                for cell in cells {
                    let mut node = native(
                        format!("{row_id}/col:{}", cell.fact.column),
                        NodeKind::Cell,
                        cell.range,
                        Some(row_id.clone()),
                    );
                    node.ranges = cell.ranges.clone();
                    node.label = Some(node.id.clone());
                    node.anchor.clone_from(&cell.fact.address);
                    node.row = Some(cell.fact.row);
                    node.column = Some(cell.fact.column);
                    node.row_span = cell.fact.row_span;
                    node.column_span = cell.fact.column_span;
                    node.display_value.clone_from(&cell.fact.display_value);
                    nodes.push(node);
                }
            }
        }
        nodes
    }
}

fn covering_range(cells: &[Cell<'_>]) -> ScalarRange {
    ScalarRange {
        start: cells.iter().map(|cell| cell.range.start).min().unwrap(),
        end: cells.iter().map(|cell| cell.range.end).max().unwrap(),
    }
}
