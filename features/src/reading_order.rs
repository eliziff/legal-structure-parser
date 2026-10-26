use serde::Deserialize;
use std::collections::HashMap;

/// A document unit and positions within it, all using the caller's offset unit.
/// Positions need not be citations; ordering depends only on document anchors.
#[derive(Deserialize)]
pub struct ReadingOrderUnit {
    pub kind: String,
    pub footnote_id: Option<usize>,
    pub footnote_refs: Vec<(usize, usize)>,
    pub item_offsets: Vec<usize>,
}

/// Port of Beaver's document reading order: a footnote follows the body up to
/// its anchor. Unanchored notes retain their input order after the body.
/// Returns (unit index, item index), without changing any original offsets.
pub fn document_reading_order(units: &[ReadingOrderUnit]) -> Vec<(usize, usize)> {
    let footnotes: HashMap<_, _> = units
        .iter()
        .enumerate()
        .filter(|(_, unit)| unit.kind == "footnote")
        .filter_map(|(index, unit)| unit.footnote_id.map(|number| (number, index)))
        .collect();
    let mut placed = vec![false; units.len()];
    let mut order = Vec::new();
    let place = |index: usize, placed: &mut [bool], order: &mut Vec<(usize, usize)>| {
        if !placed[index] {
            placed[index] = true;
            order.extend((0..units[index].item_offsets.len()).map(|item| (index, item)));
        }
    };
    for (index, unit) in units.iter().enumerate() {
        if unit.kind == "footnote" {
            continue;
        }
        placed[index] = true;
        let mut anchors = unit.footnote_refs.clone();
        anchors.sort_by_key(|(_, offset)| *offset);
        let mut next = 0;
        for (item, offset) in unit.item_offsets.iter().enumerate() {
            while next < anchors.len() && anchors[next].1 <= *offset {
                if let Some(&note) = footnotes.get(&anchors[next].0) {
                    place(note, &mut placed, &mut order);
                }
                next += 1;
            }
            order.push((index, item));
        }
        for (number, _) in &anchors[next..] {
            if let Some(&note) = footnotes.get(number) {
                place(note, &mut placed, &mut order);
            }
        }
    }
    for index in 0..units.len() {
        place(index, &mut placed, &mut order);
    }
    order
}
