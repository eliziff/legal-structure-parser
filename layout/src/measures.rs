//! A line's and a page's type and place, measured the one way every reader measures them: the
//! share of a line set bold or italic, its commonest face, the page's typical line, a recognized
//! page's off-grid sizes; and roman numerals read and written.

use crate::{line_font_size, median, quantile, Line, Page};
use std::collections::HashMap;

/// Span flags: superscript, italic, monospaced and bold, as the extraction sets them (bold and
/// italic from pdf-inspector's reading of the font's descriptor, name and stroke weight).
pub const SUPERSCRIPT: u32 = 1;
pub const ITALIC: u32 = 2;
pub const MONOSPACED: u32 = 8;
pub const BOLD: u32 = 16;

/// The share of a line's visible characters in spans carrying `flag`; none where its spans hold
/// none (a recognized line).
pub fn flag_share(line: &Line, flag: u32) -> Option<f64> {
    let (mut total, mut flagged) = (0, 0);
    for span in &line.spans {
        let count = span.text.chars().filter(|c| !c.is_whitespace()).count();
        total += count;
        if span.flags & flag != 0 {
            flagged += count;
        }
    }
    (total > 0).then(|| flagged as f64 / total as f64)
}
/// A line's commonest face and type size by characters, the size to the half point.
pub fn dominant_face(line: &Line) -> Option<(&str, f64)> {
    let mut weight: HashMap<(&str, i64), usize> = HashMap::new();
    for span in &line.spans {
        *weight.entry((span.font.as_str(), (span.size * 2.0).round() as i64)).or_default() +=
            span.text.chars().filter(|c| !c.is_whitespace()).count();
    }
    // Ties broken by name and size, so a line always reads the same way.
    weight
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|((font, size), _)| (font, size as f64 / 2.0))
}

/// How many distinct type sizes within [0.8, 1.25] of the lines' median size, by characters, fall
/// off the half-point grid: a recognizer sizes each line by its box, so a recognized page's lines
/// spread over many, where a typeset page sets every size on the grid (main dev: five or more on 46
/// of 227 recognized pages, 1 of 435 typeset). None under 200 characters.
pub fn off_grid_sizes<'a>(lines: impl IntoIterator<Item = &'a Line>) -> usize {
    let mut characters: HashMap<i64, usize> = HashMap::new();
    for line in lines {
        let count = line.text.trim().chars().count();
        let size = line_font_size(line);
        if count > 0 && size > 0.0 {
            *characters.entry((size * 10.0).round() as i64).or_default() += count;
        }
    }
    let total: usize = characters.values().sum();
    if total < 200 {
        return 0;
    }
    let mut sizes: Vec<i64> = characters.keys().copied().collect();
    sizes.sort_unstable();
    let mut seen = 0;
    let median = sizes
        .iter()
        .find(|size| {
            seen += characters[*size];
            seen * 2 >= total
        })
        .copied()
        .unwrap_or(0);
    sizes.iter().filter(|&&size| size * 100 >= median * 80 && size * 100 <= median * 125 && size % 5 != 0).count()
}
/// A page's measures for setting a line against it: its lines' slots in reading order, its
/// typical line height and type size, and its text block's edges.
pub struct PageMeasure {
    pub order: Vec<usize>,
    pub height: f64,
    pub size: f64,
    pub left: f64,
    pub right: f64,
}

impl PageMeasure {
    pub fn of(page: &Page) -> Self {
        let mut order: Vec<usize> = (0..page.lines.len()).collect();
        order.sort_by_key(|&slot| (page.lines[slot].reading_order, slot));
        let lines = &page.lines;
        Self {
            order,
            height: median(lines.iter().map(|line| (line.bbox[3] - line.bbox[1]).max(0.1)).collect()).max(0.1),
            size: median(lines.iter().filter_map(dominant_face).map(|(_, size)| size).filter(|size| *size > 0.0).collect()),
            left: quantile(lines.iter().map(|line| line.bbox[0]).collect(), 0.1),
            right: quantile(lines.iter().map(|line| line.bbox[2]).collect(), 0.9),
        }
    }
}

/// A page's columns and each line's place in its own: a two-column page's line is short against
/// the page and full against its column, and its neighbours are the lines above and below it in
/// the column, not the other column's at the same height. Columns are clusters of left edges
/// (lines within 6% of the page's width of the cluster's last edge) holding three lines or more
/// and wider than 15% of the page; a line in the margin beside its column (narrow, standing
/// outside the column's edges: a marginal note) is no neighbour of the column's lines.
pub struct Columns {
    /// Each column's left and right edge, left to right.
    pub edges: Vec<(f64, f64)>,
    /// Each line's column.
    pub of: Vec<usize>,
    /// Each line's standing in the margin beside its column.
    pub margin: Vec<bool>,
    /// Each line's neighbour above and below within its column, by vertical order.
    pub above: Vec<Option<usize>>,
    pub below: Vec<Option<usize>>,
    /// The page's typical line height, the unit its blanks are measured in.
    pub unit: f64,
}

impl Columns {
    pub fn of(page: &Page) -> Self {
        let lines = &page.lines;
        let n = lines.len();
        let width = page.width.max(1.0);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| lines[a].bbox[0].total_cmp(&lines[b].bbox[0]));
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        for slot in order {
            match clusters.last_mut() {
                Some(cluster) if lines[slot].bbox[0] - lines[*cluster.last().unwrap()].bbox[0] <= 0.06 * width => cluster.push(slot),
                _ => clusters.push(vec![slot]),
            }
        }
        let mut edges: Vec<(f64, f64)> = clusters
            .iter()
            .filter(|cluster| cluster.len() >= 3)
            .filter_map(|cluster| {
                let left = median(cluster.iter().map(|&slot| lines[slot].bbox[0]).collect());
                let right = quantile(cluster.iter().map(|&slot| lines[slot].bbox[2]).collect(), 0.9);
                (right - left > 0.15 * width).then_some((left, right))
            })
            .collect();
        if edges.is_empty() {
            let left = lines.iter().map(|line| line.bbox[0]).fold(f64::MAX, f64::min).min(width);
            let right = lines.iter().map(|line| line.bbox[2]).fold(0.0, f64::max).max(left + 0.1 * width);
            edges.push((left, right));
        }
        edges.sort_by(|a, b| a.0.total_cmp(&b.0));
        let of: Vec<usize> = lines
            .iter()
            .map(|line| {
                let middle = (line.bbox[0] + line.bbox[2]) / 2.0;
                edges
                    .iter()
                    .position(|&(left, right)| left - 0.03 * width <= middle && middle <= right + 0.03 * width)
                    .unwrap_or_else(|| {
                        (0..edges.len()).min_by(|&a, &b| (edges[a].0 - line.bbox[0]).abs().total_cmp(&(edges[b].0 - line.bbox[0]).abs())).unwrap_or(0)
                    })
            })
            .collect();
        let margin: Vec<bool> = lines
            .iter()
            .zip(&of)
            .map(|(line, &column)| {
                let (left, right) = edges[column];
                line.bbox[2] - line.bbox[0] < 0.3 * (right - left).max(1.0)
                    && (line.bbox[0] < left - 0.02 * width || line.bbox[2] > right + 0.02 * width)
            })
            .collect();
        let (mut above, mut below) = (vec![None; n], vec![None; n]);
        let mut lanes: HashMap<(usize, bool), Vec<usize>> = HashMap::new();
        for slot in 0..n {
            lanes.entry((of[slot], margin[slot])).or_default().push(slot);
        }
        for lane in lanes.values_mut() {
            lane.sort_by(|&a, &b| (lines[a].bbox[1] + lines[a].bbox[3]).total_cmp(&(lines[b].bbox[1] + lines[b].bbox[3])));
            for (at, &slot) in lane.iter().enumerate() {
                above[slot] = at.checked_sub(1).map(|prior| lane[prior]);
                below[slot] = lane.get(at + 1).copied();
            }
        }
        let unit = median(lines.iter().map(|line| line.bbox[3] - line.bbox[1]).filter(|height| *height > 0.0).collect()).max(0.1);
        Self { edges, of, margin, above, below, unit }
    }

    /// The column's width the line sits in.
    pub fn column_width(&self, slot: usize) -> f64 {
        let (left, right) = self.edges[self.of[slot]];
        (right - left).max(1.0)
    }

    /// The line's width as a share of its column's.
    pub fn width(&self, line: &Line, slot: usize) -> f64 {
        (line.bbox[2] - line.bbox[0]) / self.column_width(slot)
    }

    /// The offset of the line's middle from its column's, as a share of the column's width.
    pub fn centre(&self, line: &Line, slot: usize) -> f64 {
        let (left, right) = self.edges[self.of[slot]];
        ((line.bbox[0] + line.bbox[2]) / 2.0 - (left + right) / 2.0) / self.column_width(slot)
    }

    /// The line's indent from its column's left edge, as a share of the column's width.
    pub fn indent(&self, line: &Line, slot: usize) -> f64 {
        (line.bbox[0] - self.edges[self.of[slot]].0) / self.column_width(slot)
    }

    /// The blank over the line to its column neighbour above, in line heights; three at the top.
    pub fn gap_above(&self, lines: &[Line], slot: usize) -> f64 {
        self.above[slot].map_or(3.0, |prior| ((lines[slot].bbox[1] - lines[prior].bbox[3]) / self.unit).clamp(-3.0, 3.0))
    }

    /// The blank under the line to its column neighbour below, in line heights; three at the foot.
    pub fn gap_below(&self, lines: &[Line], slot: usize) -> f64 {
        self.below[slot].map_or(3.0, |next| ((lines[next].bbox[1] - lines[slot].bbox[3]) / self.unit).clamp(-3.0, 3.0))
    }
}

/// A roman numeral's value, its letters all capitals or all small ("xiv", "XIV"), where it is written
/// as numerals are: "iiii", "ic" and "vx" are not numerals.
pub fn roman_value(token: &str) -> Option<u32> {
    let lower = token.to_ascii_lowercase();
    if token != lower && token != token.to_ascii_uppercase() {
        return None;
    }
    let digit = |character| match character {
        'i' => Some(1),
        'v' => Some(5),
        'x' => Some(10),
        'l' => Some(50),
        'c' => Some(100),
        'd' => Some(500),
        'm' => Some(1000),
        _ => None,
    };
    let digits = lower.chars().map(digit).collect::<Option<Vec<i64>>>()?;
    let total = digits.iter().enumerate().fold(0, |total, (at, value)| {
        if digits.get(at + 1).is_some_and(|next| next > value) { total - value } else { total + value }
    });
    let value = u32::try_from(total).ok().filter(|value| (1..4000).contains(value))?;
    (roman_numeral(value) == lower).then_some(value)
}

/// A number written as a small roman numeral: 14 as "xiv".
pub fn roman_numeral(mut value: u32) -> String {
    let mut numeral = String::new();
    for (amount, letters) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while value >= amount {
            numeral.push_str(letters);
            value -= amount;
        }
    }
    numeral
}
