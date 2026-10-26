//! Order statistics over measured values (sizes, gaps, edges), shared by the parsers and the
//! extraction.

/// The median of the finite values, the middle pair averaged; 0 where there are none.
/// The upper of the two middle values where their count is even (no averaging): a measure that is
/// one of the values themselves, as the note, column and margin readers take it.
pub fn upper_median(mut values: Vec<f64>) -> f64 {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return 0.0;
    }
    let middle = values.len() / 2;
    *values.select_nth_unstable_by(middle, f64::total_cmp).1
}

pub fn median(mut values: Vec<f64>) -> f64 {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return 0.0;
    }
    let (middle, even) = (values.len() / 2, values.len() % 2 == 0);
    let (below, right, _) = values.select_nth_unstable_by(middle, f64::total_cmp);
    let right = *right;
    match below.iter().copied().max_by(f64::total_cmp) {
        Some(left) if even => (left + right) / 2.0,
        _ => right,
    }
}

/// The finite value a fraction `q` of the way through them, by nearest rank; 0 where there are none.
pub fn quantile(mut values: Vec<f64>, q: f64) -> f64 {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return 0.0;
    }
    let rank = ((values.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    *values.select_nth_unstable_by(rank, f64::total_cmp).1
}
