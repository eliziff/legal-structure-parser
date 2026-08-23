use super::*;
use crate::text::ScalarText;

const MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthoritativeTableCell {
    pub table: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub row: usize,
    pub column: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub start: usize,
    pub end: usize,
}

struct Cell<'a> {
    fact: &'a AuthoritativeTableCell,
    range: ScalarRange,
    bytes: std::ops::Range<usize>,
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
            occupied.extend((fact.row..=last_row).flat_map(|row| {
                (fact.column..=last_column).map(move |column| (fact.table, row, column, index))
            }));
            let [Some(start), Some(end)] =
                [fact.start, fact.end].map(|at| coordinates.byte_at_utf16(at))
            else {
                return Err(EngineError::invalid(
                    "Table-cell text range splits a Unicode scalar",
                ));
            };
            cells.push(Cell {
                fact,
                range: ScalarRange {
                    start: fact.start,
                    end: fact.end,
                },
                bytes: start..end,
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
            for cell in &self.cells {
                masked[cell.bytes.clone()]
                    .iter_mut()
                    .filter(|byte| **byte != b'\n')
                    .for_each(|byte| *byte = b' ');
            }
            return String::from_utf8(masked).unwrap();
        }
        let mut ranges: Vec<_> = self.cells.iter().map(|cell| cell.bytes.clone()).collect();
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        let mut masked = String::with_capacity(text.len());
        let mut at = 0;
        for range in ranges {
            let start = at.max(range.start);
            let end = start.max(range.end);
            masked.push_str(&text[at..start]);
            masked.extend(
                text[start..end]
                    .chars()
                    .map(|c| if c == '\n' { '\n' } else { ' ' }),
            );
            at = end;
        }
        masked.push_str(&text[at..]);
        masked
    }

    pub fn nodes(&self, semantic_nodes: &[StructureNode], origin_id: &str) -> Vec<StructureNode> {
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
                row.label = Some(row_id.clone());
                nodes.push(row);
                for cell in cells {
                    let mut node = native(
                        format!("{row_id}/col:{}", cell.fact.column),
                        NodeKind::Cell,
                        cell.range,
                        Some(row_id.clone()),
                    );
                    node.label = Some(node.id.clone());
                    node.anchor.clone_from(&cell.fact.address);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::normalize_javascript_whitespace;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};

    fn fact(start: usize, end: usize) -> AuthoritativeTableCell {
        AuthoritativeTableCell {
            table: 1,
            table_name: Some("Sheet".into()),
            row: 1,
            column: 1,
            row_span: None,
            column_span: None,
            address: Some("A1".into()),
            start,
            end,
        }
    }

    #[test]
    fn masks_utf16_cells_without_moving_scalars_or_line_breaks() {
        let text = "x\r\n😀 cell\ny";
        let facts = [fact(3, 10)];
        let tables = AuthoritativeTables::new(&ScalarText::new(text), &facts).unwrap();
        assert_eq!(tables.masked_text(text.to_owned()), "x\r\n      \ny");
        assert!(AuthoritativeTables::new(&ScalarText::new(text), &[fact(4, 10)]).is_err());
    }

    #[test]
    fn rejects_duplicate_and_overlapping_coordinates() {
        let first = AuthoritativeTableCell {
            row_span: Some(2),
            column_span: Some(2),
            ..fact(0, 1)
        };
        let duplicate = fact(1, 2);
        assert!(
            AuthoritativeTables::new(&ScalarText::new("ab"), &[first.clone(), duplicate])
                .err()
                .unwrap()
                .to_string()
                .contains("Duplicate")
        );
        let overlap = AuthoritativeTableCell {
            row: 2,
            column: 2,
            ..fact(1, 2)
        };
        assert!(
            AuthoritativeTables::new(&ScalarText::new("ab"), &[first, overlap])
                .err()
                .unwrap()
                .to_string()
                .contains("Overlapping")
        );
    }

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BenchNode {
        label: String,
        start: usize,
        end: usize,
        parent_label: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BenchRow {
        status: String,
        text: Option<String>,
        cells: Option<Vec<AuthoritativeTableCell>>,
        provisions: Option<Vec<BenchNode>>,
        table_nodes: Option<Vec<serde_json::Value>>,
        masked_text: Option<String>,
    }

    fn semantics(text: &str, values: Vec<BenchNode>) -> Vec<StructureNode> {
        values
            .into_iter()
            .map(|value| {
                StructureNode::new(
                    value.label,
                    NodeKind::Section,
                    ScalarRange {
                        start: value.start,
                        end: value.end,
                    },
                    "legacy",
                    Derivation::Native,
                    value.parent_label,
                )
            })
            .collect()
    }

    fn legacy_projection(
        text: &str,
        facts: &[AuthoritativeTableCell],
        semantics: &[StructureNode],
        tables: &AuthoritativeTables<'_>,
    ) -> Vec<serde_json::Value> {
        let coordinates = ScalarText::new(text);
        let mut nodes = semantics.to_vec();
        nodes.extend(tables.nodes(semantics, "native-table"));
        let depths = node_depths(&nodes);
        let by_id = nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect::<HashMap<_, _>>();
        let facts = facts
            .iter()
            .map(|fact| {
                (
                    format!("table:{}/row:{}/col:{}", fact.table, fact.row, fact.column),
                    fact,
                )
            })
            .collect::<HashMap<_, _>>();
        let mut projected = nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Table | NodeKind::Row | NodeKind::Cell))
            .map(|node| {
                let depth = depths[node.id.as_str()];
                let table = node.id.split('/').next().unwrap();
                let table_node = by_id[table];
                let table_number = table.strip_prefix("table:").unwrap();
                let table_display = table_node
                    .aliases
                    .as_ref()
                    .and_then(|aliases| aliases.first())
                    .map_or_else(
                        || format!("Table {table_number}"),
                        |name| format!("Sheet {name}"),
                    );
                let (kind, display, heading) = match node.kind {
                    NodeKind::Table => ("table", table_display, String::new()),
                    NodeKind::Row => {
                        let row = node.id.rsplit_once("/row:").unwrap().1;
                        ("row", format!("{table_display}, row {row}"), String::new())
                    }
                    NodeKind::Cell => {
                        let fact = facts[node.id.as_str()];
                        let last = fact.column + fact.column_span.unwrap_or(1) - 1;
                        let columns = if last == fact.column {
                            fact.column.to_string()
                        } else {
                            format!("{}-{last}", fact.column)
                        };
                        let bytes = coordinates.byte_at_utf16(fact.start).unwrap()
                            ..coordinates.byte_at_utf16(fact.end).unwrap();
                        (
                            "cell",
                            format!("{table_display}, row {}, column {columns}", fact.row),
                            normalize_javascript_whitespace(&text[bytes])
                                .chars()
                                .take(80)
                                .collect(),
                        )
                    }
                    _ => unreachable!(),
                };
                let mut value = serde_json::json!({
                    "kind": kind,
                    "label": node.id,
                    "display": display,
                    "heading": heading,
                    "depth": depth,
                    "start": node.range.start,
                    "end": node.range.end,
                });
                if let Some(parent) = &node.parent_id {
                    value["parentLabel"] = parent.clone().into();
                }
                value
            })
            .collect::<Vec<_>>();
        projected.sort_by_key(|value| (value["start"].as_u64(), value["depth"].as_u64()));
        projected
    }

    #[test]
    #[ignore = "corpus parity"]
    fn corpus_parity() {
        let path = std::env::var("BEAVER_TABLE_ORACLE").unwrap();
        let mut artifacts = 0;
        for line in BufReader::new(std::fs::File::open(path).unwrap()).lines() {
            let row: BenchRow = serde_json::from_str(&line.unwrap()).unwrap();
            if row.status != "table_facts" {
                continue;
            }
            let text = row.text.unwrap();
            let facts = row.cells.unwrap();
            let semantics = semantics(&text, row.provisions.unwrap());
            let tables = AuthoritativeTables::new(&ScalarText::new(&text), &facts).unwrap();
            assert_eq!(tables.masked_text(text.clone()), row.masked_text.unwrap());
            assert_eq!(
                legacy_projection(&text, &facts, &semantics, &tables),
                row.table_nodes.unwrap()
            );
            artifacts += 1;
        }
        assert_eq!(artifacts, 411);
    }

    #[test]
    #[ignore = "corpus benchmark"]
    fn corpus_benchmark() {
        let path = std::env::var("BEAVER_TABLE_ORACLE").unwrap();
        let rows = BufReader::new(std::fs::File::open(path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<BenchRow>(&line.unwrap()).unwrap())
            .filter(|row| row.status == "table_facts")
            .collect::<Vec<_>>();
        let semantics = rows
            .iter()
            .map(|row| {
                semantics(
                    row.text.as_ref().unwrap(),
                    row.provisions.as_ref().unwrap().clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut runs = Vec::new();
        for run in 0..6 {
            let hypotheses = rows
                .iter()
                .map(|row| row.text.as_ref().unwrap().clone())
                .collect::<Vec<_>>();
            let started = std::time::Instant::now();
            let outputs = rows
                .iter()
                .zip(&semantics)
                .zip(hypotheses)
                .map(|((row, semantics), hypothesis)| {
                    let text = row.text.as_ref().unwrap();
                    let tables = AuthoritativeTables::new(
                        &ScalarText::new(text),
                        row.cells.as_ref().unwrap(),
                    )
                    .unwrap();
                    (
                        tables.masked_text(hypothesis),
                        tables.nodes(semantics, "native-table"),
                    )
                })
                .collect::<Vec<_>>();
            let elapsed = started.elapsed();
            if run > 0 {
                runs.push(elapsed);
            }
            drop(outputs);
        }
        runs.sort();
        eprintln!(
            "rust artifacts={} median_ms={} runs={runs:?}",
            rows.len(),
            runs[2].as_secs_f64() * 1000.0
        );
    }
}
